# T03 ocr_images 零拷贝 + 页序契约

**type**: perf / memory
**blockers**: T01
**blocks**: T06

## 目标
`pdf/ocr.rs:46-49` — `images.chunks(n).map(|c| c.to_vec())` 全量深拷贝位图，峰值 = 2×位图。改为零拷贝消费，并固化页序契约（G2）。

## 变更
- `pdf/ocr.rs`：
  - `images.into_par_iter().chunks(chunk_size)`（rayon IndexedParallelIterator）替代 `chunks().to_vec()`
  - 若 rayon chunks 不适用，用 `Vec::split_off` 手分片，move 消费
  - **页序断言**：输出 `out` 与输入 `images` 逐位对齐校验（G2：依赖 `predict_images` 返回序=输入序，补契约测试）

## 验收
- 峰值内存 ≈ 单份位图（减半）
- golden 一致
- 页序断言测试绿

## 风险/权衡
- rayon `chunks()` 语义确认：chunk 内保序，chunk 间并行后 collect 保序
- 若 oar-ocr `predict_images` 返回序不保证 → 本 ticket 暴露，转 T06 处理（每 chunk 独立 analyzer + 手动按 chunk 归位）
