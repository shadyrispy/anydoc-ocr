# hardening — 实施索引

来源：ask-matt `/improve-codebase-architecture` + `/code-review` 全库审查（2026-08-10）。
原则：**行为不变优先于代码漂亮**；T01 golden 是唯一护栏；三通路差异（PDF 末页兜底 / OFD 列守卫 / gfm Image 重建）不得被抹平。

## DAG

```
T01 golden ─┬→ T02 去重算 ─┐
            ├→ T03 零拷贝 ─┼→ T09 Emitter → T10 Region/text_health
            ├→ T04 OFD 去双持 ─┘
            ├→ T06 OcrEngine ─→ T07 懒渲染 ─┘
            ├→ T08 表头检测 ─┘
            ├→ T11 probe 读入
            └→ T12 garbled 延迟（行为变化）
T05 独立小修
```

## 建议执行批次（blockers-first）

1. **批 A（零风险，纯收益）**：T01 → T02/T03/T04/T05 并行（互不依赖，同批）
2. **批 B（性能大项）**：T06 → T07（依赖 T01/T03）
3. **批 C（正确性）**：T08（依赖 T01）
4. **批 D（架构抽取，最后）**：T09 → T10（依赖前批 golden 全绿）
5. **尾项**：T11（低优，可弃）、T12（显式行为变化，需用户确认接受）

## 每 ticket 字段
`type / blockers / blocks / 目标 / 变更 / 验收 / 风险权衡`

## 状态

| 批次 | Ticket | 状态 | 校验 |
|---|---|---|---|
| A | T01 golden 护栏 | ✅ done | 5 非 OCR + 22 OCR 基线 |
| A | T02 page_scale 提循环外 | ✅ done | golden 全绿 |
| A | T03 OCR 分块零拷贝 | ✅ done | 峰值 2×→1× |
| A | T04 OFD 位图去双持 | ✅ done | golden 全绿 |
| A | T05 中文数字半角括号 | ✅ done | 新单测 |
| B | T06 OcrEngine 进程级缓存 | ✅ done | 同 key 只建一次 |
| B | T07 懒惰按页渲染 | ✅ done | golden 全绿 |
| C | T08 表头检测 | ✅ done | **修复真实 bug**：GJB9001C 跨页表续接行被误删，快照已更新 |
| D | T09 DocumentEmitter | ✅ done | 三通路装配统一，OCR golden 全绿 |
| D | T10 Region + text_health | ✅ done | 五元组全仓替换 + 乱码/标题前缀收敛，58 单测 + OCR golden 全绿 |
| 尾 | T11 probe mmap | ✅ done | `fs::read` → `memmap2::Mmap`（失败回落整读） |
| 尾 | T12 garbled 浅检前置 | ✅ done | 浅检命中即回退，跳过 0.3s 深检 |

**T12 实际收益修正**：票中"健康文档省 0.3s"表述有误——健康文档浅检不命中，深检仍必跑（拉丁扩展乱码兜底不可省）。真实收益 = **乱码文档**省 0.3s（本就要回退 OCR，深检结论多余）。行为语义零变化：两级判据都是"命中即回退 OCR"，或关系，顺序无关。

**收尾 clippy 清理**（新增文件范围内，机械变换零行为影响）：`emitter.rs` 文档缩进、`ocr_engine.rs` `div_ceil`、`text_health.rs` let-chain 折叠、`pdf/mod.rs` `match`→`if let`。仓库原有 lint（`timing.rs` Default 等）未动，避免超范围。

**提交前终审（两路独立 code review）修复**：
| 项 | 严重度 | 修复 |
|---|---|---|
| `table_grid.rs` `relative_row_tol` 的 `clamp` min>max panic（page_w<13.33 / pitch=NaN） | 高 | 上限 `.max(4.0)` + NaN 守卫 |
| 深检 `extract_pages_markdown` 内部整读（与 T11 矛盾） | 中 | 抽 `open_pdf_bytes`（mmap 优先），深检与探针统一走 `_mem` 版 |
| `ocr_engine::predict` 页序契约 `assert_eq`（库模式崩宿主） | 中 | 改 `anyhow::bail!` |
| `render` 无效尺寸页 `continue`（zip 页号错位） | 中 | 改 `bail!`（调用方容错回退） |
| probe `last_page - 1` u32 下溢 | 低 | `saturating_sub` |
| mmap SAFETY 注释低估 SIGBUS | 中 | 注释写明 SIGBUS 语义 + 回落整读 |

**终审确认保留（既有行为，golden 兜底，不改）**：has_header 单侧确认、跨页按列数合并、`is_control` 计 `\n\t\r`、hints 短词子串匹配、emitter `finish` 不自动 flush（三调用方均已显式 flush）。

**G1 排除**：`oar-ocr` 底层 `OrtInfer` 持 `Vec<Mutex<Session>>` + `AtomicUsize` 会话池 → `OARStructureBuilder: Sync`，无需额外同步。

**GC/内存泄露结论**：Rust 无 GC、无泄漏。真实债为重复代码 + 重复计算 + OCR 分析器每次重建，已全部收口。
