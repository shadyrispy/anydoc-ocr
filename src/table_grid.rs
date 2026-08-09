//! 文字层表格网格重建 + 跨页合并（PDF / OFD 共用）
//!
//! 各格式文字层的 TextItem / TextObject 统一映射为**文本块** `(x, y, w, h, text)`，
//! 在此完成：按 y 组行 → 行内按 x 间隙聚列 → 列 x 对齐判表（双列正文参差则拒）→
//! 合并单元格 span 推断 → `<table>` HTML。跨页续接（列数一致 / 表头去重）亦在此。
//!
//! 坐标约定：`y` **越小越靠上**（与 `reading_order` 一致）。PDF 左下原点需在调用侧
//! 翻转（`y_flip = -y`）；OFD 左上原点直接传。`x` 从左到右，`w`/`h` 为块宽高。

/// 文字层表格单元格（文本 + 几何，用于合并单元格 span 推断）。
#[derive(Clone)]
pub struct TableCell {
    pub text: String,
    pub x: f32,
    pub y: f32,
    pub h: f32,
}

/// 文字层表格网格（行×列）。
pub struct TableGrid {
    pub cols: usize,
    pub header: Vec<TableCell>,
    pub rows: Vec<Vec<TableCell>>,
}

fn empty_cell() -> TableCell {
    TableCell {
        text: String::new(),
        x: 0.0,
        y: 0.0,
        h: 0.0,
    }
}

/// 从一页文本块重建表格网格：按 y 组行、行内按 x 间隙聚列。
/// 要求：列数>=2 且 >=2 行同列数；**列 x 对齐**（同列首格 x 散布小，双列正文参差则拒）。
/// 长句/散文不再拒表（对齐 MinerU：长文本保留在单元格，判表靠列结构）。
///
/// `items`: `(x, y, w, h, text)`；`y` 越小越靠上（表头在上）。
pub fn reconstruct_table_grid(items: &[(f32, f32, f32, f32, String)], page_w: f32) -> Option<TableGrid> {
    if items.len() < 4 {
        return None;
    }
    let mut sorted: Vec<&(f32, f32, f32, f32, String)> = items.iter().collect();
    sorted.sort_by(|a, b| a.1.total_cmp(&b.1)); // y 升序：小=上（表头在顶部）
    let row_tol = 4.0_f32;
    let gap_thr = 0.012 * page_w;
    let mut rows: Vec<Vec<TableCell>> = Vec::new();
    let mut cur: Vec<&(f32, f32, f32, f32, String)> = Vec::new();
    let mut cur_y = 0.0_f32;
    for it in sorted {
        if !cur.is_empty() && (cur_y - it.1).abs() > row_tol {
            rows.push(cluster_row(&cur, gap_thr));
            cur.clear();
        }
        if cur.is_empty() {
            cur_y = it.1;
        }
        cur.push(it);
    }
    if !cur.is_empty() {
        rows.push(cluster_row(&cur, gap_thr));
    }
    // 只保留列数 >=2 的行（丢弃单格散落文本）
    let rows: Vec<Vec<TableCell>> = rows.into_iter().filter(|r| r.len() >= 2).collect();
    if rows.len() < 2 {
        return None;
    }
    let mut counts: std::collections::BTreeMap<usize, usize> = std::collections::BTreeMap::new();
    for r in &rows {
        *counts.entry(r.len()).or_default() += 1;
    }
    let (cols, col_rows) = counts.iter().max_by_key(|(_, c)| **c)?;
    let (cols, col_rows) = (*cols, *col_rows);
    if cols < 2 || col_rows < 2 {
        return None;
    }
    // 对齐到众数列数（不足补空）
    let mut aligned: Vec<Vec<TableCell>> = Vec::new();
    for r in &rows {
        let mut a = r.clone();
        a.resize(cols, empty_cell());
        aligned.push(a);
    }
    // 列 x 对齐检测：同列首格 x 散布 > 容差 → 列参差（双列正文）→ 拒
    let col_tol = (0.02 * page_w).max(10.0);
    for c in 0..cols {
        let xs: Vec<f32> = aligned
            .iter()
            .filter(|r| !r[c].text.is_empty())
            .map(|r| r[c].x)
            .collect();
        if xs.len() >= 2 {
            let mn = xs.iter().copied().fold(f32::INFINITY, f32::min);
            let mx = xs.iter().copied().fold(f32::NEG_INFINITY, f32::max);
            if mx - mn > col_tol {
                return None;
            }
        }
    }
    let header = aligned[0].clone();
    let rows = aligned[1..].to_vec();
    Some(TableGrid { cols, header, rows })
}

/// 单元格文本列表（跨页表头去重比较用）。
pub fn cell_texts(v: &[TableCell]) -> Vec<String> {
    v.iter().map(|c| c.text.clone()).collect()
}

/// 跨页续接：列数一致时合并（若下页首行 == 已有表头 → 去重表头）。
pub fn extend_table_grid(acc: &mut TableGrid, next: TableGrid) {
    if next.cols != acc.cols {
        return;
    }
    let first_matches = next
        .rows
        .first()
        .map(|r| cell_texts(r) == cell_texts(&acc.header))
        .unwrap_or(false);
    if first_matches {
        acc.rows.extend(next.rows.iter().skip(1).cloned());
    } else {
        acc.rows.extend(next.rows);
    }
}

/// 把已定型的跨页表写入其"首表页"段（保证阅读顺序）。
pub fn flush_table(segments: &mut std::collections::BTreeMap<u32, String>, grid: TableGrid, start_page: u32) {
    let e = segments.entry(start_page).or_default();
    e.push_str(&table_grid_to_html(&grid));
    e.push('\n');
    e.push('\n');
}

/// 行内按 x 间隙聚列，返回 (首格 x, y, 高, 文本) 的单元格。
fn cluster_row(items: &[&(f32, f32, f32, f32, String)], gap_thr: f32) -> Vec<TableCell> {
    let mut cells: Vec<TableCell> = Vec::new();
    let mut cluster: Vec<&(f32, f32, f32, f32, String)> = Vec::new();
    let mut x0 = 0.0_f32;
    let mut y0 = 0.0_f32;
    let mut hmax = 0.0_f32;
    for &it in items {
        if let Some(prev) = cluster.last() {
            if it.0 - (prev.0 + prev.2) > gap_thr {
                cells.push(TableCell {
                    text: join_cell_items(&cluster),
                    x: x0,
                    y: y0,
                    h: hmax,
                });
                cluster.clear();
            }
        }
        if cluster.is_empty() {
            x0 = it.0;
            y0 = it.1;
            hmax = 0.0;
        }
        cluster.push(it);
        hmax = hmax.max(it.3);
    }
    if !cluster.is_empty() {
        cells.push(TableCell {
            text: join_cell_items(&cluster),
            x: x0,
            y: y0,
            h: hmax,
        });
    }
    cells
}

/// 单元格内多个文本块拼接：仅 ASCII 字母数字间加空格（CJK 不加）。
fn join_cell_items(items: &[&(f32, f32, f32, f32, String)]) -> String {
    let mut s = String::new();
    for (i, it) in items.iter().enumerate() {
        if i > 0 {
            let a = s
                .chars()
                .last()
                .map(|c| c.is_ascii_alphanumeric())
                .unwrap_or(false);
            let b = it
                .4
                .chars()
                .next()
                .map(|c| c.is_ascii_alphanumeric())
                .unwrap_or(false);
            if a && b {
                s.push(' ');
            }
        }
        s.push_str(it.4.trim());
    }
    s
}

/// TableGrid → `<table><thead>…</thead><tbody>…</tbody></table>`。
/// 合并单元格：行内尾空 → colspan；高 cell（h > 1.5×行距）→ rowspan 并吞下方空位。
pub fn table_grid_to_html(g: &TableGrid) -> String {
    // 行距估计 = 相邻行首格 y 差的中位数（跨页边界跳变会拉大中位，抑制误判 rowspan）
    let mut ys: Vec<f32> = Vec::new();
    for row in std::iter::once(&g.header).chain(g.rows.iter()) {
        if let Some(c) = row.iter().find(|c| !c.text.is_empty()) {
            ys.push(c.y);
        }
    }
    let mut pitch = 12.0_f32;
    if ys.len() >= 2 {
        let mut gaps: Vec<f32> = ys.windows(2).map(|w| (w[0] - w[1]).abs()).collect();
        gaps.sort_by(|a, b| a.total_cmp(b));
        pitch = gaps[gaps.len() / 2].max(4.0);
    }
    let mut s = String::from("<table><thead><tr>");
    let mut c = 0;
    while c < g.header.len() {
        let cell = &g.header[c];
        let mut colspan = 1;
        while c + colspan < g.header.len() && g.header[c + colspan].text.is_empty() {
            colspan += 1;
        }
        let attr = span_attr(colspan, 1);
        s.push_str(&format!("<td{attr}>{}</td>", escape_html(&cell.text)));
        c += colspan;
    }
    s.push_str("</tr></thead><tbody>");
    let nrows = g.rows.len();
    let mut skip = vec![vec![false; g.cols]; nrows];
    for ri in 0..nrows {
        s.push_str("<tr>");
        let mut c = 0;
        while c < g.cols {
            if skip[ri][c] {
                c += 1;
                continue;
            }
            let cell = &g.rows[ri][c];
            let mut colspan = 1;
            while c + colspan < g.cols
                && g.rows[ri][c + colspan].text.is_empty()
                && !skip[ri][c + colspan]
            {
                colspan += 1;
            }
            let mut rowspan = 1;
            if !cell.text.is_empty() && cell.h > pitch * 1.5 && pitch > 0.0 {
                let est = (cell.h / pitch).round().max(1.0) as usize;
                rowspan = est.min(nrows - ri).max(1);
                if rowspan > 1 {
                    for k in (ri + 1)..(ri + rowspan).min(nrows) {
                        if c < g.cols && g.rows[k][c].text.is_empty() {
                            skip[k][c] = true;
                        }
                    }
                }
            }
            let attr = span_attr(colspan, rowspan);
            s.push_str(&format!("<td{attr}>{}</td>", escape_html(&cell.text)));
            c += colspan;
        }
        s.push_str("</tr>");
    }
    s.push_str("</tbody></table>");
    s
}

fn span_attr(colspan: usize, rowspan: usize) -> String {
    let mut a = String::new();
    if colspan > 1 {
        a.push_str(&format!(" colspan=\"{colspan}\""));
    }
    if rowspan > 1 {
        a.push_str(&format!(" rowspan=\"{rowspan}\""));
    }
    a
}

fn escape_html(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 文本块构造：(x, y, w, h, text)。
    fn blk(text: &str, x: f32, y: f32) -> (f32, f32, f32, f32, String) {
        (x, y, 20.0, 10.0, text.to_string())
    }

    #[test]
    fn grid_table_reconstructed_from_items() {
        // 3 行 × 2 列：header + 2 数据行（y 小=上，表头 y 最大在上方）
        let items = vec![
            blk("ID", 10.0, 10.0),
            blk("Name", 60.0, 10.0),
            blk("1", 10.0, 20.0),
            blk("Alice", 60.0, 20.0),
            blk("2", 10.0, 30.0),
            blk("Bob", 60.0, 30.0),
        ];
        let g = reconstruct_table_grid(&items, 200.0).expect("grid");
        assert_eq!(g.cols, 2);
        assert_eq!(cell_texts(&g.header), vec!["ID", "Name"]);
        assert_eq!(g.rows.len(), 2);
        assert_eq!(cell_texts(&g.rows[1]), vec!["2", "Bob"]);
    }

    #[test]
    fn aligned_two_column_prose_kept_as_table() {
        // 长句但列 x 对齐 → 保留为表（对齐 MinerU：长文本在单元格）
        let items = vec![
            blk("说明", 10.0, 10.0),
            blk("备注", 60.0, 10.0),
            blk("这是一段很长的说明文字，用于验证长文本保留。", 10.0, 20.0),
            blk("短", 60.0, 20.0),
            blk("继续", 10.0, 30.0),
            blk("更多", 60.0, 30.0),
        ];
        let g = reconstruct_table_grid(&items, 300.0).expect("grid");
        assert_eq!(g.cols, 2);
    }

    #[test]
    fn ragged_two_column_body_not_table() {
        // 双列正文：列 x 参差 → 拒
        let items = vec![
            blk("经研究，市人民政府决定", 10.0, 90.0),
            blk("受市生态环境部门委托", 55.0, 90.0),
            blk("一、对下列政府规章", 20.0, 80.0),
            blk("修改为：市生态环境", 70.0, 80.0),
            blk("（一）上海市放射性", 12.0, 70.0),
            blk("前增加“Ⅱ类”", 58.0, 70.0),
        ];
        assert!(reconstruct_table_grid(&items, 300.0).is_none());
    }

    #[test]
    fn single_column_not_table() {
        let items = vec![
            blk("A", 10.0, 90.0),
            blk("B", 10.0, 80.0),
            blk("C", 10.0, 70.0),
        ];
        assert!(reconstruct_table_grid(&items, 200.0).is_none());
    }

    #[test]
    fn cross_page_merge_drops_repeated_header() {
        let mut acc = TableGrid {
            cols: 2,
            header: vec![
                TableCell { text: "ID".into(), x: 10.0, y: 10.0, h: 10.0 },
                TableCell { text: "Name".into(), x: 60.0, y: 10.0, h: 10.0 },
            ],
            rows: vec![vec![
                TableCell { text: "1".into(), x: 10.0, y: 20.0, h: 10.0 },
                TableCell { text: "Alice".into(), x: 60.0, y: 20.0, h: 10.0 },
            ]],
        };
        // 下页：重复表头 + 续行
        let next = TableGrid {
            cols: 2,
            header: vec![
                TableCell { text: "ID".into(), x: 10.0, y: 10.0, h: 10.0 },
                TableCell { text: "Name".into(), x: 60.0, y: 10.0, h: 10.0 },
            ],
            rows: vec![
                vec![
                    TableCell { text: "ID".into(), x: 10.0, y: 10.0, h: 10.0 },
                    TableCell { text: "Name".into(), x: 60.0, y: 10.0, h: 10.0 },
                ],
                vec![
                    TableCell { text: "2".into(), x: 10.0, y: 20.0, h: 10.0 },
                    TableCell { text: "Bob".into(), x: 60.0, y: 20.0, h: 10.0 },
                ],
            ],
        };
        extend_table_grid(&mut acc, next);
        assert_eq!(acc.rows.len(), 2, "表头去重，仅剩 Alice/Bob");
        assert_eq!(cell_texts(&acc.rows[1]), vec!["2", "Bob"]);
    }

    #[test]
    fn html_emits_span_attributes() {
        // 高 cell（h=40 > 1.5×行距 12）→ rowspan；行内尾空 → colspan
        let g = TableGrid {
            cols: 3,
            header: vec![
                TableCell { text: "A".into(), x: 10.0, y: 10.0, h: 10.0 },
                TableCell { text: "B".into(), x: 60.0, y: 10.0, h: 10.0 },
                TableCell { text: "C".into(), x: 110.0, y: 10.0, h: 10.0 },
            ],
            rows: vec![
                // 行0: x 高格(h=40) → rowspan 吞行1 c0；c1 空 → x colspan=2
                vec![
                    TableCell { text: "x".into(), x: 10.0, y: 20.0, h: 40.0 },
                    TableCell { text: "".into(), x: 60.0, y: 20.0, h: 10.0 },
                    TableCell { text: "y".into(), x: 110.0, y: 20.0, h: 10.0 },
                ],
                vec![
                    TableCell { text: "".into(), x: 10.0, y: 30.0, h: 10.0 },
                    TableCell { text: "w".into(), x: 60.0, y: 30.0, h: 10.0 },
                    TableCell { text: "".into(), x: 110.0, y: 30.0, h: 10.0 },
                ],
            ],
        };
        let html = table_grid_to_html(&g);
        assert!(html.contains("colspan=\"2\""), "期望 colspan，got: {html}");
        assert!(html.contains("rowspan=\"2\""), "期望 rowspan，got: {html}");
    }
}
