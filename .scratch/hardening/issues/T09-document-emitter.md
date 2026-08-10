# T09 DocumentEmitter 抽取

**type**: arch（最大回归风险）
**blockers**: T01 T02 T03 T04
**blocks**: T10

## 目标
`pdf/mod.rs:226-327` / `ofd/mod.rs:187-262` / `gfm_adapter.rs:201-311` — `segments` BTreeMap + `pending` 跨页表 + flush 三处同构微差，抽公共 `DocumentEmitter`。

## 变更
- 新 `src/emitter.rs`：
  - `DocumentEmitter` 持有 `segments: BTreeMap<u32,String>` + 跨页表缓冲（`push_grid / flush / finish`）
  - 行为差异**作为钩子/配置**，不抹平：
    - PDF：`last_table_md` 末页兜底追加
    - OFD：双列守卫（`detect_column_split` 拦截）
    - gfm_adapter：Image 重建表 + `ANYDOC_DEBUG_GFM` debug 日志
- `pdf/mod.rs`/`ofd/mod.rs`/`gfm_adapter.rs` 接入

## 验收
- golden bit-identical（三通路）
- 单测：Emitter 单元（跨页合并/中断 flush/末页兜底各分支）

## 风险/权衡
- **本项目最大的"顾此失彼"陷阱**：三通路差异被抹平 → 回归。原则：行为不变优先于代码漂亮
- 先 T01 golden 后手，T08 的 header 判定已在 table_grid 内，Emitter 不重实现合并去重
- 分步：先抽 PDF 通路验证，再 OFD、gfm
