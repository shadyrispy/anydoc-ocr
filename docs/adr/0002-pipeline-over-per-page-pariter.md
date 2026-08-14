# ADR-0002: 选 render↔OCR 流水线，弃 per-page par_iter

- 状态: Accepted
- 日期: 2026-08-14
- 决策者: 架构 review

## 背景

OCR 编排有两个候选：
- **P2**：`predict` 内把 `chunks(n/threads)` 改成单页 `par_iter`，让 rayon work-stealing 均衡异构页（表格页慢、纯文字页快）。
- **P3**：render→OCR 流水线，渲染一页即送 OCR 队列，掩盖渲染延迟。

二者互斥：P3 若一页一页喂 OCR，P2 的 chunk/per-page 之别消失，P2 成废代码。

## 决策

**做 P3 流水线，不做 P2。**

## 理由

1. P3 掩盖渲染延迟是主吞吐收益（52p×100dpi 渲染耗时占比高）；P2 不掩盖渲染延迟。
2. 先做 P2 再做 P3 = P2 编排逻辑重写（顾此失彼）。
3. P3 是深模块机会：`PagePipeline` 拥有 render 线程 + OCR 池 + 背压，删除测试通过（删掉后 channel 逻辑散到 PDF/OFD 两个调用方 → 复杂度重现，说明该模块 earning its keep）。

## 后果

- 新增 `pipeline.rs`：`RenderFn` trait（`FnOnce(SyncSender<RenderItem>) -> Result<()> + Send`）+ `PagePipeline`。
- 不抽 `RenderSource` trait（PdfDocument/OfdReader 非 Send，trait 难以 clean）——改用闭包，
  调用方在闭包内 open doc + 逐页渲染，doc 不跨线程。
- PDFium `PdfDocument` 非 Send → 渲染在专属线程；OFD `&mut OfdReader` 同理。
- 有界 mpsc::sync_channel 背压（容量 = threads×2），峰值内存从 N×页图降到 ~2×页图。
- rayon scope 并发消费，BTreeMap 按 idx 回填（page_count 不预知，避免调用方提前 open doc 取 count）。
- **落地范围**：PDF 全量 OCR 通路接入（`convert_pdf` 图片型分支）。OFD 未接入——其 OcrFull 页
  在第一遍分类时已内联渲染，第二遍 OCR 无 render 可掩盖，流水收益不足；待 OFD 全图片型场景出现再评估。
- 单页 `predict_images(vec![img])` vs 批量 `predict_images(chunk)` 存在数值级差异（oar-ocr batch
  padding/resize 影响），multipage.pdf golden 已 UPDATE 重基线（P3 编排模型变更，预期）。

## 关联

- ADR-0001（不做 IR）：流水线在现有类型上跑。
- ADR-0003（GFM 流式延后）：P3 先双段，GFM 仍批量收尾。
