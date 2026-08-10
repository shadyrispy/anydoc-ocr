//! 文档装配器（T09）：统一 PDF / OFD / gfm 三通路的段表收集 + 跨页表挂起/续接/flush。
//!
//! 三通路此前各持一份 `segments: BTreeMap<u32,String>` + `pending: Option<(TableGrid,u32)>`
//! plus `flush_table/flush_grid`，逻辑同构微差。此处抽取公共部分，**页级决策（网格重建、
//! OCR 确认表、正文行构建）仍留各调用方**，差异仅 flush 格式——由 `FlushFormat` 参数化，
//! 保证行为 bit-identical（golden 兜底）。
//!
//! 注意：PDF/OFD 的表格页 `continue` 跳过正文；gfm 的表格与正文共存（不 skip）。两者
//! 都通过 `emit_grid` 管理跨页缓冲，调用方自行决定是否输出本页正文（`push_segment`）。

use std::collections::BTreeMap;

use crate::table_grid::{extend_table_grid, table_grid_to_html, TableGrid};

/// flush 写入段时的前后缀格式（两通路历史差异，必须保留）。
#[derive(Clone, Copy)]
pub enum FlushFormat {
    /// PDF/OFD：表格独占一页，`html + "\n\n"`。
    Text,
    /// gfm：表格与正文共存于首表页，`"\n\n" + html + "\n"`。
    Gfm,
}

/// 文档装配器：按页号保序收集段，跨页表格挂起并在首表页 flush。
pub struct DocumentEmitter {
    segments: BTreeMap<u32, String>,
    pending: Option<(TableGrid, u32)>,
    fmt: FlushFormat,
}

impl DocumentEmitter {
    pub fn new(fmt: FlushFormat) -> Self {
        Self {
            segments: BTreeMap::new(),
            pending: None,
            fmt,
        }
    }

    /// 处理一页的网格表：同列续接、换列 flush 旧表并挂起新表、无挂起则直接挂起。
    /// 不决定本页是否输出正文——调用方在 `continue` 前或后自行 `push_segment`。
    pub fn emit_grid(&mut self, grid: TableGrid, page: u32) {
        match self.pending.take() {
            Some((mut p, sp)) if p.cols == grid.cols => {
                extend_table_grid(&mut p, grid);
                self.pending = Some((p, sp));
            }
            Some((p, sp)) => {
                self.flush(p, sp);
                self.pending = Some((grid, page));
            }
            None => self.pending = Some((grid, page)),
        }
    }

    /// 冲掉挂起的跨页表（普通页/文档末调用）。
    pub fn flush_pending(&mut self) {
        if let Some((p, sp)) = self.pending.take() {
            self.flush(p, sp);
        }
    }

    /// 向某页段追加内容（幂等：`or_default` 保序收集）。
    pub fn push_segment(&mut self, page: u32, md: &str) {
        self.segments.entry(page).or_default().push_str(md);
    }

    fn flush(&mut self, grid: TableGrid, start_page: u32) {
        let e = self.segments.entry(start_page).or_default();
        match self.fmt {
            FlushFormat::Text => {
                e.push_str(&table_grid_to_html(&grid));
                e.push_str("\n\n");
            }
            FlushFormat::Gfm => {
                e.push_str("\n\n");
                e.push_str(&table_grid_to_html(&grid));
                e.push('\n');
            }
        }
    }

    /// 收尾：按页号升序拼接所有段并 trim 末尾空白。
    pub fn finish(self) -> String {
        let mut out = String::new();
        for (_, seg) in self.segments {
            out.push_str(&seg);
        }
        out.trim_end().to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::table_grid::{TableCell, TableGrid};

    fn cell(t: &str) -> TableCell {
        TableCell { text: t.into(), x: 0.0, y: 0.0, h: 10.0 }
    }

    fn grid(cols: usize, texts: &[&str]) -> TableGrid {
        let rows = texts
            .chunks(cols)
            .map(|c| c.iter().map(|s| cell(s)).collect())
            .collect();
        TableGrid { cols, header: vec![], rows, has_header: false }
    }

    #[test]
    fn cross_page_same_cols_merged_at_start() {
        // 两页同列表格 → 合并为一段，flush 在首表页（页 2）。
        let mut e = DocumentEmitter::new(FlushFormat::Text);
        e.emit_grid(grid(2, &["a", "b"]), 2);
        e.emit_grid(grid(2, &["c", "d"]), 3);
        e.flush_pending();
        let out = e.finish();
        // 合并为单个 <table>，含两页全部单元格；页 3 无独立段。
        assert_eq!(out.matches("<table>").count(), 1, "跨页同列合并为一表");
        assert!(out.contains("a") && out.contains("b") && out.contains("c") && out.contains("d"));
    }

    #[test]
    fn column_break_flushes_two_tables() {
        // 不同列 → 换表 flush，产生两段。
        let mut e = DocumentEmitter::new(FlushFormat::Text);
        e.emit_grid(grid(2, &["a", "b"]), 1);
        e.emit_grid(grid(3, &["x", "y", "z"]), 2);
        e.flush_pending();
        let out = e.finish();
        assert_eq!(out.matches("<table>").count(), 2, "换列产生两表");
    }

    #[test]
    fn pending_flushed_via_flush_pending() {
        // 文档末仍有挂起表 → 须显式 flush_pending（与 pdf/ofd/gfm 调用一致）。
        let mut e = DocumentEmitter::new(FlushFormat::Text);
        e.push_segment(1, "body\n");
        e.emit_grid(grid(2, &["a", "b"]), 2);
        e.flush_pending();
        let out = e.finish();
        assert!(out.contains("body") && out.contains("a"), "挂起表被 flush");
    }

    #[test]
    fn gfm_format_prefix_suffix() {
        // gfm：表格段 = "\n\n" + html + "\n"（与历史 flush_grid 一致）。
        let mut e = DocumentEmitter::new(FlushFormat::Gfm);
        e.push_segment(1, "body");
        e.emit_grid(grid(2, &["a", "b"]), 1);
        e.flush_pending();
        let out = e.finish();
        let idx = out.find("body").unwrap();
        let after = &out[idx..];
        assert!(after.starts_with("body\n\n"), "gfm 段应为 body\\n\\n+html，got: {after}");
        assert!(after.trim_end().ends_with('>'), "gfm 段以 </table> 结尾");
    }

    #[test]
    fn segments_concatenated_in_page_order() {
        let mut e = DocumentEmitter::new(FlushFormat::Text);
        e.push_segment(3, "third\n");
        e.push_segment(1, "first\n");
        e.push_segment(2, "second\n");
        let out = e.finish();
        assert_eq!(out, "first\nsecond\nthird");
    }
}

