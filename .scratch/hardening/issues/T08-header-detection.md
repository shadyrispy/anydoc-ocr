# T08 表头特征检测 + row_tol 相对化

**type**: correctness
**blockers**: T01
**blocks**: T09

## 目标
- C1：`table_grid.rs:169` — `header = aligned[0]` 无条件当表头。纯数据表（无表头行）首行被误判为表头；跨页合并时若两页首行相同 → **误去重丢行**
- C2：`table_grid.rs:64` — `row_tol=4.0` 绝对像素，dpi=300 扫描件行距大，同列 y 抖动可 >4px → 行误分

## 变更
- `table_grid.rs`：
  - `is_header_row`：短文本 / 含 编号/序号/名称/单位/备注 等关键词 / 与数据行首格样式差异 → 判表头；否则 `header` 视为首数据行（`skip(1)` 改按判定结果）
  - 跨页合并去重：仅当检测到真表头才去重
  - `row_tol` 相对化：`0.5 × 中位行距`（随调用方尺度自适应），PDF pt / OCR px 通用

## 验收
- 纯数据表跨页：首行不丢
- C.1 / 公报 / 合成跨页表 golden 不变
- 单测补：无表头纯数据表、dpi300 抖动行

## 风险/权衡
- **最大语义变化点之一**：表头判定 false-negative → 真表头被当数据行（坏）vs false-positive → 数据行被当表头去重（更坏）。关键词集合要保守，golden 守护
- 与 T09 Emitter 的合并逻辑联动：去重判定放 table_grid 内，Emitter 不重实现
