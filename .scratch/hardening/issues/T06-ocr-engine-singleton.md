# T06 OcrEngine 单例 + 模型缓存 + oar-ocr 线程安全审计

**type**: arch / perf / concurrency
**blockers**: T01 T03
**blocks**: T07 T09

## 目标
`pdf/ocr.rs:32` — `OARStructureBuilder` 每 `ocr_images` 调用重建+重载 ONNX。OFD 调 2 次 → 2 次模型加载；库模式每次 API 调用重载。建 `OcrEngine` 复用。

## 变更
- 新 `src/ocr_engine.rs`：
  - `OcrEngine::build(tier, layout)` 一次；缓存 key = (tier, layout)
  - 库模式跨文档复用；并发惰性建：`Mutex<Option<Arc<Engine>>>`，同 key 只建一次
  - 包 `render_pdf_pages`/`ocr_images` 调用面（接 T07）
- `pdf/mod.rs`/`ofd/mod.rs` 接入
- **G1 审计**：读 oar-ocr `OARStructureBuilder::predict_images` 实现，确认 `&self` 并发推理线程安全（无内部 Cell/缓存）；若否 → 每 chunk 独立 analyzer，代价=多份模型，需在 engine 缓存层面折衷

## 验收
- OFD 双 OCR 调用只载 1 次模型（计时/日志可证）
- 库模式重复 convert 零重载
- 并发 convert（多线程）无数据竞争（TSan 或压力测试）
- golden 一致

## 风险/权衡
- 缓存 key 必须含 tier+layout，防热切换误用旧 session
- engine 生命周期跨文档 → 大文档后不释放模型内存是**有意缓存**，需文档化 + 提供 `clear()` 释放口（防"优化变泄漏"反噬）
- oar-ocr Sync 审计结论决定 analyzer 是否可共享（G1）——若否，T03 页序断言与并发方案联动
