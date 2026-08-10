# T05 小修：中文数字半角括号 + escape 注释

**type**: correctness（低危）/ docs
**blockers**: 无
**blocks**: 无

## 目标
- C3：`reading_order.rs:274-284` — 中文数字编号 `一)`（半角括号）不被消费，rest 残留 `)`，标题前缀误判
- S1：`table_grid.rs:351` — `escape_html` 仅转义 `& < >`；当前 attr 均为 usize（安全），补注释声明"若 attr 拼用户文本须转义 `\"`/`'`"

## 变更
- `reading_order.rs` `parse_numbering` 中文数字分支：`cs[j]` 同时接受 `）` 与 `)`
- `table_grid.rs` `escape_html` 上方加安全注释

## 验收
- 单测补 `一) 小节` → 标题前缀命中
- 其余单测/golden 不变

## 风险/权衡
- 半角 `)` 在中文正文中极罕见作编号分隔；改动只扩大识别，不缩小 → 低风险
