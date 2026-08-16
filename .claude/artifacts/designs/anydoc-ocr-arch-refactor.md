# anydoc-ocr 架构重构 Spec

> Status: ALIGNED
> Author: user
> Last updated: 2026-08-16

## Background

anydoc-ocr（Rust，PDF/OFD→Markdown，文字层+OCR 回退）经全量代码分析暴露 13 项架构问题（双 OCR 执行路径、线程超额订阅、ofd/ 单体、回退判定散落、PDF/OFD 重复逻辑、跨页表藏在 emitter 状态机、外部错误类型受限、生产 panic 残留、配置蔓延等）。经对标 MinerU（deepwiki）与 anydoc 上游能力调研，决策维持方案 A（独立应用，anydoc 仅作 `DocKind::Other` 兜底），按 P0→P1.5/1.6→P1.7-10→P2 执行重构。

## 决策记录（用户已确认）

| 决策 | 选择 |
|---|---|
| 与 anydoc 关系 | 方案 A：自有顶层调度；不做插件化（B1）/不借其 IR 出 Markdown（B3） |
| golden 基线 | 分级策略：P0/P1.7-10 字节级不变；P1.5/1.6 理论不变，若变则逐 diff 审查、确认属潜在 bug 修复后显式 `ANYDOC_GOLDEN_UPDATE=1` 重建，diff 记入本 spec 附录 |
| 错误类型 | 自有 `anydoc_ocr::ConvertError`（kind+stage+page+detail），0.1.x 期接受 lib API breaking |
| P2 深度 | 裁剪：配置分层 + 测试补齐；**不做** trait PageAnalyzer 抽象（DocIR 即扩展缝，等真实第二后端需求） |

## In scope

- **P0-1** 统一 OCR 执行路径：`PagePipeline::run` 不再直调 `analyzer.predict_images`，复用 `OcrEngine::predict`（或把 chunk 并发下沉进 OcrEngine 并暴露 pipeline 所需入口）；pipeline 路径保证先过 `init_runtime`；背压 `BOUND_MULT` 与 intra 计算对齐，消除 2× 超额订阅。
- **P0-2** 生产 panic 清零：batch.rs:112（expect）、pipeline.rs:134/161/174/182（Mutex unwrap）、text_layer.rs:176（next_back unwrap）、reading_order.rs:353（若非 test 内）全部改 `Result`/`into_inner()` 恢复，向 ocr_engine.rs:86 既有模式看齐。
- **P0-3** detect 区分 IO 错误：`detect() -> Result<DocKind>`，打不开/读不到不再静默归为 `Other`；main 层给"文件不存在/无权限"独立提示与退出码。
- **P0-4** 依赖策略统一：原生绑定/RC/0.x 深耦合锁 `=`（anydoc、oar-ocr、ort、ofd-core 维持）；纯 Rust 库统一 `^`（image、rayon、memmap2、tempfile、zip、pdfium-render 补锁版本号后用 `^`）；pdf-inspector 维持 `^`。
- **P1.5 DocIR**：新建版面级中间表示 `src/docir/`：
  - `DocIR { pages: Vec<PageIR> }`；`PageIR { page_no, regions: Vec<Region>, source: PageSource }`；`PageSource = TextLayerPdf | TextLayerOfd | Ocr`。
  - Region 扩展 `kind + bbox + confidence`（复用/演进现有 region.rs 类型）。
  - 三源（PDF 文字层 / OFD 文字层 / OCR StructureResult）各自只产 DocIR；emitter/gfm_adapter 只消费 DocIR。
  - **跨页表合并从 emitter 状态机迁出**，改为 IR 后处理纯函数 pass（`docir/passes/cross_page_table.rs`），可单测、可开关。
- **P1.6 回退决策集中**：新建 `src/fallback.rs`：`FallbackSignal` 枚举（空文字层/坏字体浅检/坏字体深检/图片对象占比/字符数阈值/置信度探针）+ 纯函数 `decide(&[FallbackSignal]) -> Route`（`Route = TextLayer | OcrPage | OcrDoc`）；PDF 5 个 `Ok(None)` 点与 OFD 内联判定改为供信号；决策表单测。
- **P1.7 ofd/ 切分**：`ofd/render.rs`（渲染+图片对象收集）+ `ofd/text_layer.rs`（行/块/水印/乱码/ctm）+ `ofd/mod.rs`（convert_ofd 入口+质量探针），与 pdf/ 对称；纯移动为主。
- **P1.8 God fn 拆阶段**：`convert_ofd`(282 行)、`text_layer_markdown`(290 行) 按"提取→排序→表格→装配"函数化；reading_order.rs(940 行) 内部按三级降级策略拆子模块（纯移动，不改行为）。
- **P1.9 自有错误类型**：`anydoc_ocr::ConvertError { kind, stage, page, detail }` 贯穿管线；`From<anydoc::ConvertError>` 兜底通路转换；main.rs `code()` 提示语迁移；error.rs 删除对 anydoc 私有 map_error 的镜像。
- **P1.10 batch 回归调度层**：`BatchConverter` 内部走统一 convert 入口，跨文档 pipeline 成为 convert 的实现细节；固化返回结构类型。
- **P2（裁剪）**：
  - 配置分层 `ConvertRequest { render: RenderConfig{dpi}, ocr: OcrConfig{tier,layout}, parallel: ParallelConfig{page_parallel, ort_intra} }`；`Default` 修掉 `dpi=0` 陷阱（默认 100.0）；`threads` 双语义拆开；force_flags 维持独立。
  - 测试补齐：fallback 决策表、DocIR 后处理 pass（跨页表头去重/续接）、quality 门控、detect 魔数表。

## Out of scope

- anydoc 插件化（B1：上游 PR 注册格式/OCR hook）与 IR 借用（B3：产出 anydoc::Document 用其序列化器）。
- trait PageAnalyzer / 后端注册抽象；VLM 后端。
- 输出格式/风格变化（GFM 语义冻结，golden 守护）。
- 性能优化专项（仅 P0-1 顺带的线程模型修正）。
- `ConvertOptions` 之外的新公开功能面（无新 CLI 参数语义）。

## Assumptions

- 构建环境具备 ORT/PDFium 原生库或可动态获取；若沙箱无法编译，则以静态审查 + 后续环境验证替代（风险标注）。
- 0.1.x 期 lib API breaking 可接受（下游仅自家 CLI + golden 测试）。
- golden 非 OCR 子集（22 快照中不触发模型下载的部分）可在 CI 跑；OCR 子集按需。

## Solution sketch

数据流（目标态）：

```
main → convert(detect 分流)
  ├─ pdf::  文字层 ─┐
  ├─ ofd::  文字层 ─┼→ DocIR（版面级：page+region+bbox+confidence+source）
  ├─ OCR pipeline ─┘     ├─ pass: cross_page_table（跨页表合并，纯函数）
  │                      ├─ pass: reading_order（沿用现有纯函数）
  └─ anydoc（兜底，不动） └→ emitter/gfm_adapter 消费 DocIR → GFM
fallback.rs: 三源各自收集 FallbackSignal → decide() → Route
错误：自有 ConvertError{kind,stage,page} 贯穿，边界即 API
```

实施顺序与验证门：每阶段结束跑 `cargo test --test golden`（非 OCR 子集）+ `cargo clippy`；P0/P1.7-10 要求快照 SHA 零变化；P1.5/1.6 若变化走 diff 审查流程。

## Edge cases & risks

| Category | Notes |
|---|---|
| Boundary | 混合文档（部分页文字层/部分页图片）Route 需页级粒度；OFD 无深检信号时决策表须有默认臂 |
| Failure modes | 沙箱缺原生库导致无法编译验证；ORT init 顺序（pipeline 路径）改变可能影响线程池行为 |
| Risks | DocIR 迁移动 17 文件中约 10 个，回归面大→分级 golden 策略 + 阶段化落地控制 |
| Mitigation | 每阶段独立可停；P1.5 先建 IR 并迁移消费者，再迁生产者；emitter 跨页表状态机删除前先有 pass 等价单测 |

## Acceptance criteria

**P0**
- AC-1 `grep -rn "unwrap()\|expect(" src/ --include="*.rs"` 生产路径（非 `#[cfg(test)]`）仅剩 ocr_engine 认可的 into_inner 恢复模式，计数为 0 panic 点。
- AC-2 pipeline 路径与 OcrEngine::predict 共享同一推理入口；`ANYDOC_TIMINGS=1` 下两路径计时字段一致。
- AC-3 detect 对不存在文件返回 Err(IO)，main 输出独立提示；对真未知格式仍返回 Other。
- AC-4 `cargo test --test golden`（非 OCR 子集）全部通过且 22 快照 SHA 不变。
- AC-5 Cargo.toml 无策略不一致的版本约束（按 P0-4 规则）。

**P1.5**
- AC-6 `src/docir/` 存在，emitter/gfm_adapter 签名只依赖 DocIR（不依赖 pdf/ofd 内部类型或 StructureResult）。
- AC-7 跨页表合并以独立 pass 存在且有≥3 个单测（续接、表头去重、非续表打断）。
- AC-8 golden 不变；若变，diff 审查记录于本 spec 附录并显式 UPDATE 重建。

**P1.6**
- AC-9 `fallback::decide` 为纯函数，PDF/OFD 通路无内联回退判定残留（grep 无散落 Ok(None) 路由）。
- AC-10 决策表单测覆盖全信号组合的边界（≥10 用例）。

**P1.7/1.8**
- AC-11 `ofd/` 三文件结构与 pdf/ 对称；`convert_ofd`/`text_layer_markdown` 主函数体 ≤100 行。
- AC-12 golden 快照 SHA 不变。

**P1.9/1.10**
- AC-13 `lib.rs` 公开 `anydoc_ocr::ConvertError`；`error.rs` 无 anydoc 私有镜像；错误可携带 stage+page。
- AC-14 batch 不再直调 `pdf::text_layer_markdown`/`convert_pdf_ocr` 内部 API（走 convert 层入口或公开固化契约）。
- AC-15 `cargo test` 全绿（含 error_classification、batch_golden）。

**P2**
- AC-16 `ConvertRequest` 三组配置落地，Default 的 dpi=100.0；threads 双语义拆为两个字段。
- AC-17 fallback/DocIR pass/quality/detect 均有对应单测文件或模块。

**审计**
- AC-18 编码完成后走 dev-code-review，发现项按严重度修复或记录。

## Open questions

None.

## Core entities (ontology)

| Entity | Type | Key fields | Relationship |
|---|---|---|---|
| DocIR | struct | pages: Vec\<PageIR\> | 三源产出，passes 消费 |
| PageIR | struct | page_no, regions, source | DocIR 组成 |
| Region | struct | kind, bbox, confidence, content | PageIR 组成（演进自 region.rs） |
| PageSource | enum | TextLayerPdf/TextLayerOfd/Ocr | PageIR 来源标注 |
| FallbackSignal | enum | 空层/浅检/深检/图片占比/字符数/置信度 | decide() 输入 |
| Route | enum | TextLayer/OcrPage/OcrDoc | decide() 输出 |
| ConvertRequest | struct | render/ocr/parallel | P2 配置分层 |

## Interview metadata

- Mode: default（3 waves）
- Waves: 3
- Final ambiguity: ~10%
- Status: PASSED

### Clarity breakdown

| Dimension | Score | Weight | Weighted |
|---|---|---|---|
| Goal | 0.95 | 0.40 | 0.38 |
| Scope | 0.85 | 0.25 | 0.21 |
| AC | 0.90 | 0.25 | 0.225 |
| Context | 0.95 | 0.10 | 0.095 |

### 附录：golden diff 审查记录

**2026-08-16 P0 前置：环境基线漂移（非代码变更）**
- `tests/samples/multipage.pdf`：want `7efab541a9e7f295` → got `2f45036a47d513c6`
- `tests/samples/real_table.pdf`：want `680669511b4eeac4` → got `135c5cc17a02b7c2`
- 原因：两样本虽标 needs_ocr=false，但可疑表格页会渲染+版面 OCR 确认；沙箱 pdfium（bblanchon latest）与基线生成环境的 pdfium 渲染像素存在差异 → OCR 框微小漂移。已目检 real_table 输出：表格结构/内容完整无损，属环境漂移非行为回归；批/单输出一致性（batch_golden 同 hash）。
- 处置：按分级策略在改码**前**重建这 2 个快照为本环境基线，后续阶段以本环境基线为回归锚点。
- 附注：rustc 1.92 → 1.97.1（oar-ocr 0.9.1 要求 ≥1.95）；ORT 1.20.1 x64 动态链接 + pdfium 预编译库置于 `third_party/`（已 .gitignore 惯例，本地不入库）。
- 附注 2：batch_golden 在 `--test-threads=1` 下通过逻辑断言（仅上述 2 快照漂移），默认并行线程下曾出现 SIGTRAP 启动崩溃——疑似并行测试 × ORT 初始化竞态，P0-1 统一路径后复验。
