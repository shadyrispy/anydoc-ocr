# ADR-0007：基于图像质量的 OCR 档位路由

- 状态：拟议
- 日期：2026-08-15
- 决策者：anydoc-ocr 维护者
- 上下文：ADR-0005（批处理/跨文档流水线）、ADR-0006（错误处理统一）落地后，实测扫描件识别发现默认 `tiny/100dpi` 对中文小字（目录点线、宋体小字）易错；用户需显式 `--ocr-tier small` 才能无瑕疵。需自动化档位选择。

## 背景

nuaa.pdf（GJB 9001C 扫描件，37 页）实测：

| 配置 | 耗时 | 关键错字 | 结论 |
|------|------|----------|------|
| tiny/100（当前默认） | 132s | 8 | 目次→蘭窗、术语→來语 等 |
| tiny/150 | 180s | 5 | 关键词纠正，残留范園/职贵 |
| tiny/200 | 244s | 5 | 反而退化（蘭言/木语） |
| small/100 | 266s | 1 | 仅"职贵"一处 |
| small/200 | 396s | 1 | 与 small/100 相同，DPI 无收益 |

**反直觉发现**：tiny 模型在 200 DPI 下识别反而退化（超训练分布）。最优组合为 `small/100`（无瑕疵前提最快）或 `tiny/150`（速度优先可容忍少量错字）。

**问题**：单一默认档位无法兼顾"清晰扫描件要快"与"污染扫描件要准"。需按图像质量自动路由。

## 相关研究与业界实践

| 来源 | 方法 | 借鉴点 |
|------|------|--------|
| Gaikwad 2026（arXiv:2604.25176） | Laplacian 方差三级路由 HIGH/MEDIUM/LOW | 与本 ADR 思路完全一致，工业界主流 |
| Krithika 2026（Frontiers） | 12 特征（锐度/边缘/结构）+ XGBoost 预测 OCR CER | 特征设计参考，但 ML 回归器过重 |
| Kopytek 2024（MDPI） | 多阈值二值化一致性，免跑 OCR 预测质量 | 轻量思路，但二值化对扫描质量分级不如 Laplacian 直接 |
| DeQA-Doc 2026 | MOS 质量分与 CER 强相关（SRCC 0.54-0.66） | 概念验证：质量分可预测 OCR 精度 |
| doc-quality（开源） | 23 analyzers，含 FFT 频谱/区域均匀性 | 过重，本 ADR 取其 Laplacian + noise 子集 |

**共识**：Laplacian 方差 + 噪声估计 → 阈值分级路由，是工业界标准做法。

## 决策

### D1：用 imageproc 现成卷积 + 手写统计

**不引入新图像质量专用库**，复用 `imageproc`（MIT，image-rs 生态姊妹库，项目已依赖 `image=0.25`，版本匹配）。

imageproc 提供：
- `filter::laplacian_filter(&GrayImage) -> Image<Luma<i16>>` —— Laplacian 3×3 卷积
- `gradients::sobel_gradients(&GrayImage) -> Image<Luma<u16>>` —— Sobel 梯度幅值
- `filter::gaussian_blur_f32(&GrayImage, f32) -> Image<Luma<f32>>` —— 高斯模糊（噪声残差用）

手写部分（~50 行）：对卷积结果做统计（variance/mean/min-max）。

**否决的备选**：
- `purecv`：WIP 明确"not yet stable"
- `oximedia-quality`：video quality，带入 VMAF/SSIM 重依赖
- `photo-qa`：依赖 candle-core/candle-nn（PyTorch 替代），过重
- `zenanalyze`：AGPL-3.0 传染，license 不可接受

### D2：四指标定义（阈值采用业界标准参数）

| 指标 | 衡量 | 实现 | imageproc 复用 |
|------|------|------|---------------|
| **归一化** Laplacian 方差 | 模糊度（核心） | `laplacian_filter` 方差 ÷ 原图纹理方差 | ✅ 卷积复用 |
| 局部噪声方差 | 噪点污染 | 原图 - `gaussian_blur_f32(σ=1)` 残差的均值 | ✅ 模糊复用 |
| 对比度 | 灰度动态范围 | `(max - min) / 255.0` | 手写（1 行） |
| 平均锐度 | 边缘清晰度 | `sobel_gradients` 结果的均值 | ✅ 卷积复用 |

**关键修正：Laplacian 方差必须归一化**。原始方差依赖分辨率（4K 照片 vs 100DPI 扫描件量纲不同），跨 DPI 比较会失真。归一化方式：

```rust
let lap_norm = laplacian_var / (texture_var + 1e-6);
// texture_var = 原图灰度方差 np.var(gray)
```

参考：imageguard（MIT 库）用此归一化产出 0-100 分，OCR 阈值 60。

**阈值参数（业界标准，多源交叉验证）**：

| 原始 Laplacian 方差 | 归一化值（约） | 质量 | OCR 适用性 | 来源 |
|---------------------|--------------|------|-----------|------|
| >500 | >2.5 | 很清晰 | 优秀 | changeimageto.com, woteq.com |
| 200–500 | 1.0–2.5 | 可接受 | 良好 | 同上 |
| 50–200 | 0.25–1.0 | 偏软 | 精度下降 | 同上 |
| <50 | <0.25 | 模糊 | 不适用 | 同上 |
| **默认阈值 100** | ~0.5 | 分界线 | 通用经验值 | CSDN, woflowinc/img-blur-check |

**手写总量**：统计函数 ~50 行 + 归一化 ~5 行 + 路由分级 ~30 行 = ~85 行新增代码。

### D3：三级路由策略

```
质量分 = 阈值树判定（非加权求和，避免指标间量纲耦合）

HIGH   (清晰扫描): tiny/100    → 132s  (当前默认，快)
MEDIUM (轻度污染): tiny/150    → 180s  (DPI 提升，覆盖小字)
LOW    (重污染/小字): small/100 → 266s  (tier 升级，无瑕疵)
```

**阈值树设计**（保守偏向，宁可升级；阈值取业界标准参数）：
1. 若归一化 Laplacian < 0.5 → LOW（明显模糊，对应原始方差 <100）
2. 否则若 噪声方差 > 50.0 或 对比度 < 0.3 → MEDIUM（有污染）
3. 否则 → HIGH

阈值依据：Laplacian 0.5 归一化值 = 业界通用 100 原始方差点（4 源交叉验证）；噪声/对比度阈值待 7 样本微调但量级已定。

**否决的备选**：
- 加权求和总分：四指标量纲不同，权重需训练数据标定，7 样本不足
- 深度 NR-IQA（BRISQUE/NIQE）：需训练模型，依赖重
- 按页混用档位（同文档内 HIGH 页用 tiny、LOW 页用 small）：实现复杂，跨页结果一致性差，首版不做

### D4：按文档评估，非按页

取前 3 页质量分的中位数代表全文档，路由到单一档位。

**否决按页评估的原因**：
- 同文档混用 tier 会导致跨页输出风格不一致（tiny 和 small 的版面分析差异）
- 实现复杂度高（每页独立路由 + 结果合并）
- 前 3 页中位数已足够代表文档整体质量（目次页通常质量最低，若目次页都不触发 LOW，正文页更不会）

**抽样而非全页的原因**：37 页全算约 5.5-7.4s 额外开销，占 OCR 总耗时 4-6%；前 3 页抽样约 0.45-0.6s，占 <0.5%，代表性足够。

### D5：可禁用，保证测试确定性

CLI 新增 `--quality-route auto|off`：
- `auto`（默认）：质量路由生效
- `off`：退回 `--ocr-tier`/`--dpi` 显式指定，保证 golden 测试确定性

golden 测试固定 `--quality-route off`，避免路由阈值漂移导致快照失效。

## 实施计划

### 阶段 1：质量评估器（src/quality.rs）

新增 `src/quality.rs`：
- `QualityMetrics { laplacian_var, noise_var, contrast, sharpness }`
- `fn assess(img: &GrayImage) -> QualityMetrics` —— 调用 imageproc 卷积 + 手写统计
- `enum QualityTier { High, Medium, Low }`
- `fn route(m: &QualityMetrics) -> QualityTier` —— 阈值树
- `impl QualityTier { fn tier(&self) -> OcrTier; fn dpi(&self) -> u32 }` —— 映射到档位

Cargo.toml 新增 `imageproc = "0.25"`（与 image 0.25 版本对齐）。

验证：`cargo build` + 单元测试（用 fixtures 样本断言分级正确）。

### 阶段 2：集成到流水线（pipeline.rs）

- OCR 前插入质量评估步骤：取前 3 页渲染图，调 `assess` + `route`
- 按路由结果设置 `OcrTier` 和 `dpi`
- `--quality-route off` 时跳过评估，用 `--ocr-tier`/`--dpi` 显式值

验证：`cargo build` + nuaa.pdf 实测路由到 LOW（small/100）+ cjrb.pdf 不走路由（有文字层）。

### 阶段 3：阈值验证与测试

- 用 7 样本（nuaa/cjrb/sthj + encrypted/corrupt PDF/docx）验证业界标准阈值（Laplacian 0.5 / noise 50 / contrast 0.3）
- 噪声/对比度阈值如需微调，记入配置常量；Laplacian 阈值固定取业界标准
- 新增 `tests/quality_routing.rs`：断言 nuaa→LOW、sthj→MEDIUM/HIGH、清晰样本→HIGH
- golden 测试加 `--quality-route off` 固定参数
- 文档：CLI `--help` 说明 auto/off 语义

验证：全量 `cargo test` + 三样本实测对比路由前后耗时与准确率。

## 风险与缓解

| 风险 | 影响 | 缓解 |
|------|------|------|
| 阈值过拟合 | 业界标准阈值可能不适合中文扫描件 | Laplacian 阈值取 4 源交叉验证的业界标准（100/0.5）；噪声/对比度阈值可调 + `--quality-route off` 兜底 |
| 路由误判代价不对称 | HIGH 误判为 LOW → 浪费 2 倍时间；LOW 误判为 HIGH → 识别错误 | 阈值偏向保守，宁可升级不可降级 |
| golden 快照失效 | 质量路由改变默认行为 | golden 固定 `--quality-route off`，路由有独立测试 |
| imageproc 依赖膨胀 | 编译时间增加 | imageproc 是 image-rs 生态轻量库，无重依赖；与 image 0.25 版本对齐 |
| 前 3 页不代表全文档 | 长文档后段质量突变 | 首版接受此风险；未来可按需扩展为分桶抽样 |

## 非目标

- 不做深度 NR-IQA（BRISQUE/NIQE/MANIQA）—— 依赖重，7 样本不足训练
- 不做按页混用档位 —— 实现复杂，跨页一致性差
- 不做二值化一致性法 —— 对扫描质量分级不如 Laplacian 直接
- 不自动判断"是否扫描件" —— 文字层检测已有（text_layer.rs），无文字层即走 OCR 路径，质量路由仅作用于 OCR 路径

## 引用

- [Gaikwad 2026 - Adaptive OCR Pipeline with Laplacian Three-Tier Routing](https://arxiv.org/pdf/2604.25176)
- [Krithika 2026 - OCR-based Document Image Quality Assessment](https://public-pages-files-2025.frontiersin.org/journals/signal-processing/articles/10.3389/frsip.2026.1779355/pdf)
- [Kopytek 2024 - Binary Image Quality Assessment for OCR Prediction](https://www.mdpi.com/2076-3417/14/22/10275)
- [DeQA-Doc - Quality Scores Predict OCR Accuracy](https://github.com/ByronWilliamsCPA/DeQA-Doc/blob/main/research/papers/06_ocr_iqa_correlation/paper.md)
- [doc-quality - Document Image Quality Checker](https://github.com/loekj/doc-quality)
- [imageproc - filter::laplacian_filter](https://github.com/image-rs/imageproc/blob/main/src/filter/mod.rs)
- [imageproc - gradients::sobel_gradients](https://rustdocs.webschool.au/imageproc/gradients/fn.sobel_gradients.html)
