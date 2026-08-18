//! 跨页网格表合并 pass（P1.5/AC-7）：从 emitter 挂起状态机迁出为 IR 后处理纯函数。
//!
//! 语义与旧 `DocumentEmitter::emit_grid`/`flush_pending` 逐调用点镜像（golden 守护）：
//! - 连续页（页序列相邻）各有 [`RegionKind::Grid`] 且**列数一致** → 续接合并
//!   （表头重复去重由 `table_grid::extend_table_grid` 承担：`has_header` 且续页
//!   首行 == 表头 → 丢弃续页首行）；
//! - 列数变化 → 前表定格在首表页，新表挂起；
//! - 无 Grid 的页（正文页/OCR 确认表页/成品块页）→ 打断挂起，前表定格；
//! - 文档末仍有挂起 → 定格。
//!
//! 合并结果写回首表页的 regions（Grid 区块），续页的 Grid 区块移除——渲染层
//! 只见"每张跨页表一个 Grid、落在首表页"，无需任何跨页状态。

use crate::docir::DocIR;
use crate::region::{Region, RegionKind};
use crate::table_grid::{TableGrid, extend_table_grid};

/// 执行跨页表合并（原地修改 `doc`）。
pub fn run(doc: &mut DocIR) {
    // (目标页下标, 合并后的 grid)：状态机产出的定格表，最后回写。
    let mut finalized: Vec<(usize, TableGrid)> = Vec::new();
    // 挂起表：(累积 grid, 首表页下标)。
    let mut pending: Option<(TableGrid, usize)> = None;

    for (i, page) in doc.pages.iter_mut().enumerate() {
        // 拆出本页 Grid 区块（producer 保证每页至多 1 个；多 Grid 时逐个走
        // 状态机，与旧 emitter 逐 emit 语义一致），其余区块保留原位。
        let mut grids: Vec<TableGrid> = Vec::new();
        let mut rest: Vec<Region> = Vec::with_capacity(page.regions.len());
        for r in page.regions.drain(..) {
            match r.kind {
                RegionKind::Grid(g) => grids.push(g),
                _ => rest.push(r),
            }
        }
        page.regions = rest;
        let had_grid = !grids.is_empty();

        for g in grids {
            match pending.take() {
                // 同列续接（表头去重在 extend_table_grid 内）。
                Some((mut acc, at)) if acc.cols == g.cols => {
                    extend_table_grid(&mut acc, g);
                    pending = Some((acc, at));
                }
                // 换列：前表定格，新表挂起。
                Some((acc, at)) => {
                    finalized.push((at, acc));
                    pending = Some((g, i));
                }
                None => pending = Some((g, i)),
            }
        }

        // 本页无 Grid → 打断挂起（正文页/表格 HTML 页/成品块页均如此，
        // 与旧通路在非网格页 flush_pending 的调用点一一对应）。
        if !had_grid
            && let Some((acc, at)) = pending.take()
        {
            finalized.push((at, acc));
        }
    }
    // 文档末定格。
    if let Some((acc, at)) = pending.take() {
        finalized.push((at, acc));
    }

    // 回写：定格表挂回首表页 regions 末尾（渲染按 kind 分流，位置无关紧要）。
    for (at, grid) in finalized {
        if let Some(page) = doc.pages.get_mut(at) {
            page.regions.push(
                Region::new(0.0, 0.0, 0.0, 0.0, String::new()).with_kind(RegionKind::Grid(grid)),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::docir::PageSource;
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

    /// AC-7 用例 1（续接）：连续两页同列网格 → 合并为首表页单表，续页 Grid 移除。
    #[test]
    fn continuation_same_cols_merged_at_first_page() {
        let mut doc = DocIR {
            pages: vec![
                grid_page(0, grid(2, &[], &[&["a", "b"], &["c", "d"]], false)),
                grid_page(1, grid(2, &[], &[&["e", "f"]], false)),
            ],
        };
        run(&mut doc);
        let g0 = grid_of(&doc.pages[0]).expect("首表页保有合并表");
        assert_eq!(g0.rows.len(), 3, "两页行数续接：a/b/c/d + e/f");
        assert_eq!(grid_of(&doc.pages[1]), None, "续页 Grid 已移除");
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
        let g0 = grid_of(&doc.pages[0]).expect("合并表在首表页");
        let texts: Vec<&str> = g0.rows.iter().map(|r| r[0].text.as_str()).collect();
        assert_eq!(texts, vec!["1", "2"], "续页重复表头被去重，数据行保留");
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
        assert_eq!(grid_of(&doc.pages[0]).unwrap().cols, 2);
        assert_eq!(grid_of(&doc.pages[1]).unwrap().cols, 3, "换列各自成表");
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
        assert_eq!(grid_of(&doc.pages[0]).unwrap().rows.len(), 1, "前表定格页 0");
        assert_eq!(grid_of(&doc.pages[2]).unwrap().rows.len(), 1, "后表定格页 2");
        assert!(grid_of(&doc.pages[1]).is_none(), "正文页无 Grid");
    }

    /// 文档末挂起表定格在首表页（对齐旧通路末尾 flush_pending）。
    #[test]
    fn trailing_pending_finalized_at_end() {
        let mut doc = DocIR {
            pages: vec![grid_page(0, grid(2, &[], &[&["a", "b"]], false))],
        };
        run(&mut doc);
        assert!(grid_of(&doc.pages[0]).is_some(), "末页挂起表仍定格");
    }

    /// 续页其余区块（正文行）不受 Grid 移除影响。
    #[test]
    fn non_grid_regions_preserved() {
        let mut doc = DocIR {
            pages: vec![crate::docir::PageIR {
                page_no: 0,
                regions: vec![
                    Region::new(0.0, 10.0, 0.0, 5.0, "行"),
                    Region::new(0.0, 0.0, 0.0, 0.0, String::new())
                        .with_kind(RegionKind::Grid(grid(2, &[], &[&["a", "b"]], false))),
                ],
                source: PageSource::Ocr,
            }],
        };
        run(&mut doc);
        assert_eq!(doc.pages[0].regions.len(), 2, "正文行 + 定格 Grid");
        assert!(matches!(doc.pages[0].regions[0].kind, RegionKind::Body));
    }
}
