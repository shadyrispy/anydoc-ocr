# T10 Region 新类型 + text_health 模块

**type**: arch / DRY
**blockers**: T09
**blocks**: 无

## 目标
- A4：`(f32,f32,f32,f32,String)` 元组全仓贯穿 → 新类型 `Region`，挂 `.page_w()`/`.is_full()`（消魔法数 `0.92`/`0.08` 重复于 `reading_order.rs:37,112`）
- A2：`apply_title_prefixes` 3 实现（pdf/mod.rs:345 / ofd/mod.rs:312 / gfm_adapter.rs:335，签名各异）→ 统一 `for_lines(lines, &title_hints)`
- A3：garbled/furniture 重复（`looks_garbled` pdf / `is_garbled_text` ofd / `is_repeated_furniture` pdf）→ `text_health` 模块

## 变更
- `src/region.rs`、`src/text_health.rs`；`reading_order.rs`/`table_grid.rs`/`gfm_adapter.rs`/`pdf`/`ofd` 接入

## 验收
- golden 一致
- 魔法数消除，单测覆盖 Region 方法
- 编译通过（类型强制确保全仓替换完整）

## 风险/权衡
- 改动面大（全仓类型替换）→ 分步：先 Region 替换（编译器兜底），再 text_health，最后统一 apply_title_prefixes
- 在 T09 之后做：Emitter 已统一装配面，此 ticket 只统一类型/工具，不碰行为
- 注意 `is_full` 的 `0.92`/`0.08` 与 `detect_column_split` 的 `0.92`/`0.08` 口径必须一致（同一常量）
