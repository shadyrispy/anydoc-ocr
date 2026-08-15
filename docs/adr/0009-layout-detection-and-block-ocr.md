# ADR-0009：版面检测模型 + 分块 OCR（对标 MinerU pipeline）

- 状态：拟议
- 日期：2026-08-15
- 决策者：anydoc-ocr 维护者
- 上下文：ADR-0008（图像直提）落地后，nuaa4 等 OOM 解决。但 GJB 9001C 对比测试暴露根本差距——anydoc-ocr 准确率 77%，MinerU 99.6%，差距源于无版面检测模型导致水印/页眉混入正文（插入错误 3330 vs 287）。

## 背景

### 实测对比（同一扫描版 sdoac_scan.pdf 37 页，同一文字版 GT）

| 指标 | anydoc tiny/100 | anydoc small/150 | MinerU pipeline |
|------|----------------|-----------------|-----------------|
| 原始 CER | 23.56% | 22.60% | **0.40%** |
| 准确率 | 76.44% | 77.40% | **99.60%** |
| 替换错误 | 428 | 800 | **19** |
| 插入(OCR多检) | 3330 | 2820 | **287** |
| 删除(OCR漏检) | 2312 | 1889 | **40** |

### 根因分析

OCR 引擎同源（均 PP-OCR），差距不在单字识别（替换错误我们反而更少），在**版面层**：

| 环节 | MinerU | anydoc-ocr | 差距影响 |
|------|--------|-----------|---------|
| 版面检测 | DocLayout-YOLO，10+ 类块 | 无，启发式 92% 宽度 | 水印/页眉无法过滤 → 插入 3330 |
| OCR 粒度 | 分块裁剪 OCR | 整页 OCR | 水印文字当正文识别 |
| 阅读序 | 基于版面块 bbox | 最大间隙启发式 | 目录点线拆碎 → 段数 222 vs 177 |

## 决策

### D1：接入 DocLayout-YOLO 版面检测模型

**模型选择**：DocLayout-YOLO-small（ONNX 格式）
- 检测类别：title / text / image / table / header / footer / formula / list / caption
- 模型大小：~50MB
- CPU 推理：~200ms/页（ort 已是依赖）
- License：AGPL-3.0 → **需确认**，若不可接受改用 PaddleStructure（Apache-2.0）

**否决的备选**：
- LayoutLMv3：依赖 transformers，过重
- PaddleStructure PP-Structure：Apache-2.0 可接受，但模型更大 ~200MB
- 纯启发式增强（扩展 92% 宽度规则）：无法处理水印（非整宽）、页码（小区域），天花板低

### D2：分块 OCR 替代整页 OCR

版面检测产出 `Vec<LayoutBlock>` 后，按块裁剪 text 区域独立 OCR：

```rust
// 新增 src/layout.rs
pub struct LayoutBlock {
    pub block_type: BlockType, // Title/Text/Image/Table/Header/Footer/Formula
    pub bbox: BBox,            // [x1,y1,x2,y2] 归一化坐标
}

pub fn detect_layout(img: &RgbImage) -> Result<Vec<LayoutBlock>> {
    // ort 加载 DocLayout-YOLO → 预处理 → 推理 → NMS → 解析
}

// pipeline 改造：整页 OCR → 分块 OCR
for block in detect_layout(&page_img)? {
    if matches!(block.block_type, BlockType::Header | BlockType::Footer) {
        continue; // 丢弃页眉页脚
    }
    if block.block_type == BlockType::Text || block.block_type == BlockType::Title {
        let crop = crop_bbox(&page_img, &block.bbox);
        let text = engine.predict_images(vec![crop]);
        results.push((block, text));
    }
    // Image/Table/Formula 暂跳过或走专用通路
}
```

**收益**：水印不在 text block 内 → 不被 OCR；页眉页脚块直接丢弃 → 插入错误从 3330 降到预估 <500。

### D3：阅读序基于版面块 bbox

替代当前 `reading_order.rs` 的最大间隙启发式：

```rust
// 版面块按 [列 → y] 排序
// 列检测：聚类 block.center_x，列间 gap > 5% 页宽
// 列内：按 y_min 升序
```

保留 `reading_order.rs` 作为降级（版面检测失败时回退）。

### D4：middle_json 中间格式（P2，非首版必须）

借鉴 MinerU 引入结构化中间表示，后处理基于结构化数据而非字符串拼接。首版可暂不做，直接拼 markdown。

## 实施计划

### 阶段 1：版面检测模型集成（src/layout.rs）

- `cargo add ort`（已间接依赖，确认版本）
- 下载 DocLayout-YOLO-small ONNX 模型到 `models/`
- 实现 `detect_layout(img) -> Vec<LayoutBlock>`：预处理（resize 640×640 + normalize）→ 推理 → NMS → 坐标反映射
- 确认 AGPL-3.0 许可证兼容性，不兼容则换 PaddleStructure

验证：单页推理 + bbox 可视化（保存标注图人工确认）。

### 阶段 2：分块 OCR 改造（pipeline.rs）

- `PagePipeline::run` 内：渲染页 → `detect_layout` → 按 block 裁剪 → 分块 OCR
- header/footer 块丢弃
- text/title 块按阅读序拼接
- 降级：版面检测失败 → 回退整页 OCR（当前逻辑）

验证：nuaa.pdf 插入错误从 3330 降到 <1000；golden 测试更新。

### 阶段 3：阅读序升级（reading_order.rs）

- 版面块 bbox 列检测 + 列内 y 排序
- 保留最大间隙启发式作为降级
- 目录页作为一个 text block 整体处理（解决点线拆碎）

验证：目录段数从 222 降到接近 GT 的 177。

### 阶段 4：对比测试

- sdoac_scan.pdf 重跑，对比 CER 从 23.56% 降到 <5%
- 耗时对比：+200ms/页 × 37 页 = +7.4s（可接受）
- golden 测试更新

## 风险与缓解

| 风险 | 影响 | 缓解 |
|------|------|------|
| DocLayout-YOLO AGPL-3.0 | 许可证传染 | 确认后若不兼容改用 PaddleStructure (Apache-2.0) |
| 模型推理增加 200ms/页 | 耗时 +7.4s/37页 | 版面检测与 OCR 可流水线并发（渲染→检测→OCR 三段） |
| CPU 内存压力 | 模型加载 ~200MB 常驻 | ort 模型单例，与 OcrEngine 共享线程池 |
| 分块 OCR 增加调用次数 | 每页 5-20 块 vs 1 次整页 | 批量裁剪后一次性 predict_images（oar_ocr 支持批量） |
| 版面检测误检 | 块边界不准导致文字截断 | bbox 外扩 5% padding + 降级回退整页 OCR |

## 非目标

- 不做公式识别（MFD/MFR）—— 成本高收益窄，P4 再考虑
- 不做表格 SLANet 结构重建 —— 保留现有网格启发式
- 不做 VLM 后端 —— MinerU 的 vlm-engine 需 GPU，我们面向 CPU
- 不做 middle_json —— 首版直接拼 markdown，P2 再结构化

## 引用

- [MinerU Pipeline Backend - DeepWiki](https://deepwiki.com/OpenDataLab/MinerU/2.1-pipeline-backend)
- [MinerU Layout Detection & Reading Order - DeepWiki](https://deepwiki.com/OpenDataLab/MinerU/3.1-layout-detection-and-reading-order)
- [MinerU OCR Engine - DeepWiki](https://deepwiki.com/OpenDataLab/MinerU/3.3-ocr-engine)
- [DocLayout-YOLO - GitHub](https://github.com/Pear-Orange/DocLayout-YOLO)
- [imageguard - blur + 5 quality signals](https://github.com/vipul510-web/imageguard)
- [Laplacian variance threshold standard - changeimageto.com](https://www.changeimageto.com/blog/how-to-filter-blurry-images-before-ocr.html)
