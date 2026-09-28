//! 跨页网格表合并 pass（P1.5/AC-7）：从 emitter 挂起状态机迁出为 IR 后处理纯函数。
//!
//! 语义与旧 `DocumentEmitter::emit_grid`/`flush_pending` 逐调用点镜像（golden 守护）：
//! - 连续页（页序列相邻）各有 [`RegionKind::Grid`] 且**列数一致** → 续接合并
//!   （表头重复去重由 `table_grid::extend_table_grid` 承担：`has_header` 且续页
//!   `rows[0]` == 表头 → 丢弃该行。注意它**只看 `rows`**，续页 `header` 槽里那行
//!   是无条件不进合并结果的——producer 已把该页首行放进 header 槽，故"去重"只在
//!   续页把表头印了两行时才可达。入库件 `tests/samples/cross_page_table.pdf`
//!   就是那两行的形态，理由见其生成器 docstring）；
//! - 列数变化 → 前表定格在首表页，新表挂起；
//! - 无 Grid 的页（正文页/OCR 确认表页/成品块页）→ 打断挂起，前表定格；
//! - 文档末仍有挂起 → 定格。
//!
//! 合并结果：首表页的 Grid 区块换成合并后的表，**续页的 Grid 区块保留原位**但打
//! [`continues_prev`](crate::region::Region::continues_prev) 标记（#6 第 3 步）——渲染层按标记跳过
//! 它，输出与本 pass 原先"物理删除续页区块"逐字节相同（由单测
//! `absorbed_stub_renders_identically_to_deletion` 钉住），而投影层（#10/#11）能在
//! 续页上看到"这张表在此页续接"而不是"表格消失"。
//!
//! 为什么留占位而不是删掉：MinerU 的 `continues_prev` 挂在**续页那个块**上
//! （`docvortex/schema.py:506-509`、`content/table/document.py:104`），删掉区块就没
//! 有承载它的对象了——旧通路删除是 emitter 状态机的产物，不是刻意的语义选择。
//! 注意两侧的**内容口径差别**：MinerU 里带标记的块自己仍带正文，本仓标记块保留的是
//! producer 原始 grid（未去重、未并入），合并结果只在首表页那份里。

use crate::docir::DocIR;
use crate::region::RegionKind;
use crate::table_grid::{TableGrid, extend_table_grid};

/// 执行跨页表合并（原地修改 `doc`）。
pub fn run(doc: &mut DocIR) {
    // (目标页下标, 目标页内位置, 合并后的 grid)：状态机产出的定格表，最后回写。
    // 页内位置取代旧通路的"push 到 regions 末尾"——渲染按 kind 分阶段取块，同 kind
    // 之间只相对顺序要紧，而定格顺序与首表位置顺序一致（详见模块头注），
    // 原位覆盖与末尾追加**渲染等价**，原位形态才让占位块的相对位置有意义。
    let mut finalized: Vec<(usize, usize, TableGrid)> = Vec::new();
    // 挂起表：(累积 grid, 首表页下标, 首表区块在本页的位置)。
    let mut pending: Option<(TableGrid, usize, usize)> = None;

    for (i, page) in doc.pages.iter_mut().enumerate() {
        // 逐区块走状态机（producer 保证每页至多 1 个 Grid；多 Grid 时逐个处理，
        // 与旧 emitter 逐 emit 语义一致）。非 Grid 区块完全不动。
        let mut had_grid = false;
        for (pos, r) in page.regions.iter_mut().enumerate() {
            let kind = std::mem::replace(&mut r.kind, RegionKind::Body);
            let RegionKind::Grid(g) = kind else {
                r.kind = kind;
                continue;
            };
            // 已是占位块（前一次 run 的吸收产物）→ 不参与状态机，也不计入
            // "本页有 Grid"。不这么做的话重复 run 会把**已并入**的行再并一次
            // （首表页那份 acc 里已含这些行）→ 行数翻倍。producer 永不置此标记，
            // 故该分支只可能出现在二次 run 上（本 pass 因此可重复调用）。
            if r.is_continues_prev() {
                r.kind = RegionKind::Grid(g);
                continue;
            }
            had_grid = true;
            match pending.take() {
                // 同列续接（表头去重在 extend_table_grid 内）：本页区块被吸收 →
                // **原位保留 + 打标记**（#6 第 3 步），内容仍是 producer 原始 grid。
                Some((mut acc, at, at_pos)) if acc.cols == g.cols => {
                    let own = g.clone();
                    extend_table_grid(&mut acc, g);
                    pending = Some((acc, at, at_pos));
                    r.kind = RegionKind::Grid(own);
                    r.continues_prev = Some(true);
                }
                // 换列：前表定格（写回它自己的位置），新表挂起（区块原位保留自己的
                // grid，定格时被合并结果覆盖——同一区块，合并结果的起点）。
                Some((acc, at, at_pos)) => {
                    finalized.push((at, at_pos, acc));
                    r.kind = RegionKind::Grid(g.clone());
                    pending = Some((g, i, pos));
                }
                None => {
                    r.kind = RegionKind::Grid(g.clone());
                    pending = Some((g, i, pos));
                }
            }
        }

        // 本页无 Grid → 打断挂起（正文页/表格 HTML 页/成品块页均如此，
        // 与旧通路在非网格页 flush_pending 的调用点一一对应）。
        if !had_grid
            && let Some((acc, at, at_pos)) = pending.take()
        {
            finalized.push((at, at_pos, acc));
        }
    }
    // 文档末定格。
    if let Some((acc, at, at_pos)) = pending.take() {
        finalized.push((at, at_pos, acc));
    }

    // 回写：定格表覆盖回首表区块所在位置（合并结果只存在于首表页那份里）。
    for (at, at_pos, grid) in finalized {
        if let Some(region) = doc
            .pages
            .get_mut(at)
            .and_then(|page| page.regions.get_mut(at_pos))
        {
            region.kind = RegionKind::Grid(grid);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::docir::PageSource;
    use crate::region::Region;
    use crate::table_grid::TableCell;

    fn cell(t: &str) -> TableCell {
        TableCell {
            text: t.into(),
            x: 0.0,
            y: 0.0,
            h: 10.0,
        }
    }

    /// 构造网格表：`has_header` 时首行进 header（去重判定依据）。
    fn grid(cols: usize, header: &[&str], rows: &[&[&str]], has_header: bool) -> TableGrid {
        TableGrid {
            cols,
            header: header.iter().map(|s| cell(s)).collect(),
            rows: rows.iter().map(|r| r.iter().map(|s| cell(s)).collect()).collect(),
            has_header,
        }
    }

    fn page(page_no: u32, regions: Vec<Region>) -> crate::docir::PageIR {
        crate::docir::PageIR {
            page_no,
            regions,
            source: PageSource::TextLayerPdf,
            dims: crate::docir::PageDims::default(),
        }
    }

    fn body_page(page_no: u32) -> crate::docir::PageIR {
        page(page_no, vec![Region::new(0.0, 100.0, 0.0, 10.0, "正文")])
    }

    fn grid_page(page_no: u32, g: TableGrid) -> crate::docir::PageIR {
        page(
            page_no,
            vec![Region::new(0.0, 0.0, 0.0, 0.0, String::new()).with_kind(RegionKind::Grid(g))],
        )
    }

    /// 取页面上唯一的 Grid（无则 None）。
    fn grid_of(page: &crate::docir::PageIR) -> Option<&TableGrid> {
        page.regions.iter().find_map(|r| match &r.kind {
            RegionKind::Grid(g) => Some(g),
            _ => None,
        })
    }

    /// 取页面上**未被吸收**的 Grid（跳过 `continues_prev` 占位块）——即渲染层看得到
    /// 的那份。#6 第 3 步后 `grid_of` 会同时命中占位块，断言合并结果须用它。
    fn live_grid_of(page: &crate::docir::PageIR) -> Option<&TableGrid> {
        page.regions.iter().find_map(|r| match &r.kind {
            RegionKind::Grid(g) if !r.is_continues_prev() => Some(g),
            _ => None,
        })
    }

    /// 取页面上的占位块（已并入前页表格的续页区块）。
    fn stub_of(page: &crate::docir::PageIR) -> Option<&Region> {
        page.regions.iter().find(|r| r.is_continues_prev())
    }

    /// AC-7 用例 1（续接）：连续两页同列网格 → 合并为首表页单表，续页区块
    /// **原位保留并打标记**（#6 第 3 步；旧通路是删除，删除后无块可挂标记）。
    #[test]
    fn continuation_same_cols_merged_at_first_page() {
        let mut doc = DocIR {
            pages: vec![
                grid_page(0, grid(2, &[], &[&["a", "b"], &["c", "d"]], false)),
                grid_page(1, grid(2, &[], &[&["e", "f"]], false)),
            ],
        };
        run(&mut doc);
        let g0 = live_grid_of(&doc.pages[0]).expect("首表页保有合并表");
        assert_eq!(g0.rows.len(), 3, "两页行数续接：a/b/c/d + e/f");
        let stub = stub_of(&doc.pages[1]).expect("续页区块原位保留为占位");
        assert_eq!(stub.continues_prev, Some(true));
        // 占位块内容 = producer 原始 grid（未并入、未去重）——不是空块、也不是合并结果。
        assert_eq!(
            grid_of(&doc.pages[1]).unwrap().rows.len(),
            1,
            "占位块保留续页自己的行"
        );
    }

    /// #6 第 3 步的核心等价性：**留占位 + 渲染跳过** 与 **物理删除区块** 两种 IR
    /// 形状必须渲染逐字节相同（这条成立，"不触碰字节契约"才不是口头承诺）。
    #[test]
    fn absorbed_stub_renders_identically_to_deletion() {
        let pages = || {
            vec![
                grid_page(0, grid(2, &[], &[&["a", "b"]], false)),
                // 续页带一行正文，验证跳过不会顺手吃掉别的块或改分隔符
                crate::docir::PageIR {
                    page_no: 1,
                    regions: vec![
                        Region::new(0.0, 100.0, 0.0, 10.0, "续页正文"),
                        Region::new(0.0, 0.0, 0.0, 0.0, String::new())
                            .with_kind(RegionKind::Grid(grid(2, &[], &[&["e", "f"]], false))),
                    ],
                    source: PageSource::Ocr,
                    dims: crate::docir::PageDims::default(),
                },
            ]
        };
        let mut kept = DocIR { pages: pages() };
        run(&mut kept);
        assert!(stub_of(&kept.pages[1]).is_some(), "pass 后应有占位块");
        // 另一形状：同样跑 pass，再把占位块删掉（= 旧通路的 IR 形状）。
        let mut deleted = DocIR { pages: pages() };
        run(&mut deleted);
        for page in &mut deleted.pages {
            page.regions.retain(|r| !r.is_continues_prev());
        }
        assert!(stub_of(&deleted.pages[1]).is_none(), "对照形状已无占位块");
        assert_eq!(
            kept.render(),
            deleted.render(),
            "留占位与删除必须渲染同形（含续页正文与表格的分隔符）"
        );
        // 占位块确实没被渲染出来：跨页表只输出一份 HTML，且续页行只出现一次。
        let md = kept.render();
        assert_eq!(md.matches("<table>").count(), 1, "跨页表只输出一份，got: {md}");
        assert_eq!(md.matches("<td>e</td>").count(), 1, "续页行经合并只出现一次，got: {md}");
    }

    /// 重复 run 幂等：占位块不再参与状态机，否则已并入的行会被**再并一次**。
    #[test]
    fn run_is_idempotent_over_stubs() {
        let mut doc = DocIR {
            pages: vec![
                grid_page(0, grid(2, &[], &[&["a", "b"]], false)),
                grid_page(1, grid(2, &[], &[&["e", "f"]], false)),
            ],
        };
        run(&mut doc);
        let once = live_grid_of(&doc.pages[0]).unwrap().rows.len();
        let md_once = doc.render();
        run(&mut doc);
        assert_eq!(live_grid_of(&doc.pages[0]).unwrap().rows.len(), once, "二次 run 行数不变");
        assert_eq!(doc.render(), md_once, "二次 run 输出不变");
        assert_eq!(stub_of(&doc.pages[1]).map(|r| r.continues_prev), Some(Some(true)));
    }

    /// AC-7 用例 2（表头去重）：has_header 且续页首行 == 表头 → 续页首行丢弃。
    #[test]
    fn repeated_header_dropped_on_continuation() {
        let mut doc = DocIR {
            pages: vec![
                grid_page(0, grid(2, &["编号", "名称"], &[&["1", "甲"]], true)),
                grid_page(1, grid(2, &["编号", "名称"], &[&["编号", "名称"], &["2", "乙"]], false)),
            ],
        };
        run(&mut doc);
        let g0 = live_grid_of(&doc.pages[0]).expect("合并表在首表页");
        let texts: Vec<&str> = g0.rows.iter().map(|r| r[0].text.as_str()).collect();
        assert_eq!(texts, vec!["1", "2"], "续页重复表头被去重，数据行保留");
        // 去重只发生在合并结果里；占位块保留 producer 原始两行（含重复表头）。
        assert_eq!(grid_of(&doc.pages[1]).unwrap().rows.len(), 2, "占位块未被去重");
    }

    /// AC-7 用例 3（非续表打断·换列）：列数变化 → 两表各自定格，不合并。
    #[test]
    fn column_change_breaks_into_two_tables() {
        let mut doc = DocIR {
            pages: vec![
                grid_page(0, grid(2, &[], &[&["a", "b"]], false)),
                grid_page(1, grid(3, &[], &[&["x", "y", "z"]], false)),
            ],
        };
        run(&mut doc);
        assert_eq!(live_grid_of(&doc.pages[0]).unwrap().cols, 2);
        assert_eq!(live_grid_of(&doc.pages[1]).unwrap().cols, 3, "换列各自成表");
        assert!(stub_of(&doc.pages[1]).is_none(), "换列不是续接，不打标记");
    }

    /// AC-7 用例 4（非续表打断·正文页）：中间正文页打断续接，两侧各自成表。
    #[test]
    fn body_page_breaks_pending_grid() {
        let mut doc = DocIR {
            pages: vec![
                grid_page(0, grid(2, &[], &[&["a", "b"]], false)),
                body_page(1),
                grid_page(2, grid(2, &[], &[&["e", "f"]], false)),
            ],
        };
        run(&mut doc);
        assert_eq!(live_grid_of(&doc.pages[0]).unwrap().rows.len(), 1, "前表定格页 0");
        assert_eq!(live_grid_of(&doc.pages[2]).unwrap().rows.len(), 1, "后表定格页 2");
        assert!(grid_of(&doc.pages[1]).is_none(), "正文页无 Grid");
        assert!(stub_of(&doc.pages[2]).is_none(), "被打断的后表是首表，不打标记");
    }

    /// 文档末挂起表定格在首表页（对齐旧通路末尾 flush_pending）。
    #[test]
    fn trailing_pending_finalized_at_end() {
        let mut doc = DocIR {
            pages: vec![grid_page(0, grid(2, &[], &[&["a", "b"]], false))],
        };
        run(&mut doc);
        assert!(live_grid_of(&doc.pages[0]).is_some(), "末页挂起表仍定格");
    }

    /// 续页其余区块（正文行）不受吸收影响，且定格表**原位**写回（不是追加到末尾）。
    #[test]
    fn non_grid_regions_preserved_and_grid_stays_in_place() {
        let mut doc = DocIR {
            pages: vec![crate::docir::PageIR {
                page_no: 0,
                regions: vec![
                    Region::new(0.0, 10.0, 0.0, 5.0, "行"),
                    Region::new(0.0, 0.0, 0.0, 0.0, String::new())
                        .with_kind(RegionKind::Grid(grid(2, &[], &[&["a", "b"]], false))),
                ],
                source: PageSource::Ocr,
                dims: crate::docir::PageDims::default(),
            }],
        };
        run(&mut doc);
        assert_eq!(doc.pages[0].regions.len(), 2, "正文行 + 定格 Grid（原位，非新增）");
        assert!(matches!(doc.pages[0].regions[0].kind, RegionKind::Body));
        assert!(matches!(doc.pages[0].regions[1].kind, RegionKind::Grid(_)), "Grid 仍在原位下标 1");
    }

    /// 三页连续续接：第 2、3 页都打标记，合并结果只在首表页（占位块各留自己的行）。
    #[test]
    fn multi_page_chain_marks_every_continuation() {
        let mut doc = DocIR {
            pages: vec![
                grid_page(0, grid(2, &[], &[&["a", "b"]], false)),
                grid_page(1, grid(2, &[], &[&["c", "d"]], false)),
                grid_page(2, grid(2, &[], &[&["e", "f"]], false)),
            ],
        };
        run(&mut doc);
        assert_eq!(live_grid_of(&doc.pages[0]).unwrap().rows.len(), 3, "三页续成一张表");
        for p in 1..=2 {
            assert!(stub_of(&doc.pages[p]).is_some(), "页 {p} 应为占位块");
            assert_eq!(grid_of(&doc.pages[p]).unwrap().rows.len(), 1, "占位块保留本页行");
        }
    }
}
