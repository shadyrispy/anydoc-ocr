# T02 page_scale 热路径去重算

**type**: perf（纯优化，零行为变化）
**blockers**: T01
**blocks**: T09

## 目标
`gfm_adapter.rs:141,247` — `page_scale(page)` 在 `for r in regs` 内每 region 重算（遍历全 region+layout），O(n²)。每页算 1 次。

## 变更
- `gfm_adapter.rs`：
  - `reconstruct_image_table`：`scale = page_scale(page)` 提到循环外，传入 `norm_membership`
  - `structure_results_to_gfm` 正文 region 循环：同上去重

## 验收
- golden bit-identical（零行为变化）
- 500 region 页：O(250k)→O(500) 迭代，计时可观测

## 风险/权衡
- 无。纯缓存重算结果，不改变判定逻辑
