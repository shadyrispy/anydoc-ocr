# T11 probe 末页表整文件读入

**type**: memory
**blockers**: T01
**blocks**: 无

## 目标
`pdf/mod.rs:454` — `probe_last_page_table` 用 `std::fs::read(path)` 把整个 PDF 载入内存，仅探末页表。500MB PDF → 500MB 缓冲。

## 变更
- `pdf/mod.rs` `probe_last_page_table`：
  - 复用前面已 open 的句柄/文件路径，避免二次全读
  - 若 `extract_tables_in_regions_mem` 必须 `&[u8]` → 改 mmap（`memmap2`）或接受一次性读（权衡：末页探针本身低频，收益有限）

## 验收
- golden 一致；大 PDF 不再产生整读峰值（可选：mmap 后 RSS 观测）

## 风险/权衡
- **低优**：仅末页探针路径，一次 500MB 峰值 vs 全流程内存。若 mmap 引入新依赖/平台差异，可降级为"接受现状 + 文档化"——不要为小收益增加复杂度
