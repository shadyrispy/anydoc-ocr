# T04 OFD 位图去双持

**type**: memory
**blockers**: T01
**blocks**: T09

## 目标
`ofd/mod.rs:103,106,123 + 145,148` — 第一遍 `pages` 已持 `RgbImage`，第二遍 `img.clone()` 又复制；探针页/图片页峰值 2×。

## 变更
- `ofd/mod.rs`：
  - `PageData::OcrFull/Probe` 的 `img` 在第一遍记录后**所有权转移**到第二遍消费集合（`pages` 不再双持）
  - 遍历改用 `into_iter` 消费 `pages`（第三遍仍需遍历 → 先拆出 `images` 所有权再装配，或 `Option<RgbImage>` take）

## 验收
- 峰值内存减半（OFD 图片型文档）
- golden 一致

## 风险/权衡
- 第三遍装配需要 `pages` 的 `lines`/类型信息 → 用 `Option<RgbImage>` `take()` 转移，避免重构装配循环
- 保持 `page_no` 递增语义不变
