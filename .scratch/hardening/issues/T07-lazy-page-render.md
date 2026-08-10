# T07 懒惰按索引渲染

**type**: perf / memory
**blockers**: T01 T06
**blocks**: T09

## 目标
`pdf/mod.rs:194` — `render_pdf_pages` 渲**所有页**进 `Vec<RgbImage>`，仅 `suspicious` 子集送 OCR（52p 文档渲 52 页只 OCR 3 页）。改为按需渲指定索引。

## 变更
- `pdf/render.rs`：`render_pdf_pages(path, dpi, page_indices: &[u32]) -> Vec<RgbImage>`（PDFium 支持按索引渲）
- `text_layer_markdown`：只渲 `suspicious` 页
- `convert_pdf` 图片型全量路径：仍全渲，但接入 T06 engine 懒渲染/分段
- 保留 `idx = p-1` 映射校验（已有 `by_page.contains_key` + `idx < images.len()`）

## 验收
- 52p 文档：只渲 3 可疑页（计时/日志可证），峰值内存大降
- golden 一致（尤其跨页表续接不受页序影响）

## 风险/权衡
- PDFium 按索引渲 vs 全量渲的尺寸/坐标必须一致 → 单测断言
- 段序：`suspicious` 升序渲染，输出按页号 zip 回 `BTreeMap`，防缺页错位
- 与 T06 接口绑定：渲染+OCR 都按 engine 生命周期走，避免重复开 pdfium 单例
