//! 文字层表格网格重建 + 跨页合并（PDF / OFD 共用）
//!
//! 各格式文字层的 TextItem / TextObject 统一映射为 `Region`（`x_min/x_max/y_min/
//! y_max/text`，由调用侧以 `Region::from_top_left(x, y, w, h, text)` 传入块），
//! 在此完成：按 y 组行 → 行内按 x 间隙聚列 → 列 x 对齐判表（双列正文参差则拒）→
//! 合并单元格 span 推断 → `<table>` HTML。跨页续接（列数一致 / 表头去重）亦在此。
//!
//! 坐标约定：`y` **越小越靠上**（与 `reading_order` 一致）。PDF 左下原点需在调用侧
//! 翻转（`y_flip = -y`）；OFD 左上原点直接传。`x` 从左到右，块宽高 = `x_max-x_min`/
//! `y_max-y_min`。

use crate::region::Region;

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
    /// 首行是否为真表头（决定跨页合并是否去重；纯数据表为 false，防丢首行）。
    pub has_header: bool,
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
/// `items`: `Region`（`x_min/x_max/y_min/y_max/text`）；`y` 越小越靠上（表头在上）。
pub fn reconstruct_table_grid(items: &[Region], page_w: f32) -> Option<TableGrid> {
    reconstruct_grid(items, page_w, false)
}

/// 宽松版（Image 块补救用）：列对齐用**中位数 + 离群率**，容忍 OCR 框起始抖动
/// （扫描件表格如 C.1 条款号列 x 漂移 ~16px，绝对散布判据会误拒）。PDF 文字层
/// 仍用严格版（绝对散布），防双列正文误判。调用方需自行做 2 列长文本兜底。
pub fn reconstruct_table_grid_tolerant(items: &[Region], page_w: f32) -> Option<TableGrid> {
    reconstruct_grid(items, page_w, true)
}

fn reconstruct_grid(items: &[Region], page_w: f32, tolerant: bool) -> Option<TableGrid> {
    if items.len() < 4 {
        return None;
    }
    let mut sorted: Vec<&Region> = items.iter().collect();
    sorted.sort_by(|a, b| a.y_min.total_cmp(&b.y_min)); // y 升序：小=上（表头在顶部）
    let gap_thr = 0.012 * page_w;
    // 行距自适应：row_tol 用相对值（0.5×中位行距），随 dpi/尺度变化，避免
    // 高 dpi 扫描件同列 y 抖动 > 绝对 4px 时行误分（C2）。
    let row_tol = relative_row_tol(items, page_w);
    let rows = group_rows(&sorted, row_tol, gap_thr);
    // 只保留列数 >=2 的行（丢弃单格散落文本）
    let rows: Vec<Vec<TableCell>> = rows.into_iter().filter(|r| r.len() >= 2).collect();
    if rows.len() < 2 {
        return None;
    }
    let mut counts: std::collections::BTreeMap<usize, usize> = std::collections::BTreeMap::new();
    for r in &rows {
        *counts.entry(r.len()).or_default() += 1;
    }
    // 列数选择：**最大且行数>=2** 的列数（非众数）。OCR 常漏检小字段列
    // （如 C.1 条款号列），众数会偏小（3）而非真实列数（4）；取最大完整行
    // 的列数，缺列行 resize 补空。孤立多列行（行数<2）跳过，防撑大。
    let mut cols = 0usize;
    let mut col_rows = 0usize;
    for (k, c) in counts.iter().rev() {
        if *k >= 2 && *c >= 2 {
            cols = *k;
            col_rows = *c;
            break;
        }
    }
    if cols < 2 || col_rows < 2 {
        return None;
    }
    // 按 x 归列（非顺序补空）：缺列行（OCR 漏检小字段列，如 C.1 条款号）按
    // 每块 x 归到最近的列模板中心——2 块行归 c0/c3、3 块行归 c0/c2/c3，而非
    // 顺序填充导致列错位。列模板 = 完整行（len==cols）每列 x0 的中位数。
    let complete: Vec<&Vec<TableCell>> = rows.iter().filter(|r| r.len() == cols).collect();
    let mut col_centers = vec![0.0_f32; cols];
    for c in 0..cols {
        let mut xs: Vec<f32> = complete.iter().map(|r| r[c].x).collect();
        xs.sort_by(|a, b| a.total_cmp(b));
        if xs.is_empty() {
            return None;
        }
        col_centers[c] = xs[xs.len() / 2];
    }
    let mut aligned: Vec<Vec<TableCell>> = Vec::new();
    for r in &rows {
        let mut a: Vec<TableCell> = (0..cols).map(|_| empty_cell()).collect();
        for cell in r {
            // 归到最近列中心；同列冲突（多块）保留 x 最小的块，其余忽略
            let mut best = 0usize;
            let mut best_d = f32::INFINITY;
            for (ci, cx) in col_centers.iter().enumerate() {
                let d = (cell.x - cx).abs();
                if d < best_d {
                    best_d = d;
                    best = ci;
                }
            }
            if a[best].text.is_empty() || cell.x < a[best].x {
                a[best] = cell.clone();
            }
        }
        aligned.push(a);
    }
    // 列 x 对齐检测：数据行同列首格 x 散布 > 容差 → 列参差（双列正文）→ 拒。
    // 排除表头行：表头常合并列/居中（如 C.1 合并表头覆盖多列），x 与数据列不对齐。
    let col_tol = (0.02 * page_w).max(10.0);
    for c in 0..cols {
        let xs: Vec<f32> = aligned
            .iter()
            .skip(1) // 排除表头行：表头常合并列/居中，x 与数据列不对齐是正常的
            .filter(|r| !r[c].text.is_empty())
            .map(|r| r[c].x)
            .collect();
        if xs.len() >= 2 {
            if tolerant {
                // 宽松：中位数 ± col_tol 内占比 >=70% 即视为对齐（容忍 OCR 离群）
                let mut v = xs.clone();
                v.sort_by(|a, b| a.total_cmp(b));
                let med = v[v.len() / 2];
                let outliers = v.iter().filter(|x| (**x - med).abs() > col_tol).count();
                if outliers * 100 > v.len() * 30 {
                    return None;
                }
            } else {
                // 严格：绝对散布（双列正文参差 → 拒）
                let mn = xs.iter().copied().fold(f32::INFINITY, f32::min);
                let mx = xs.iter().copied().fold(f32::NEG_INFINITY, f32::max);
                if mx - mn > col_tol {
                    return None;
                }
            }
        }
    }
    let header = aligned[0].clone();
    let body = aligned[1..].to_vec();
    // C1：首行是否真表头？纯数据表首行当数据行，跨页合并不去重（防丢首行）。
    let has_header = is_header_row(&header, &body);
    Some(TableGrid {
        cols,
        header,
        rows: body,
        has_header,
    })
}

/// 单元格文本列表（跨页表头去重比较用）。
pub fn cell_texts(v: &[TableCell]) -> Vec<String> {
    v.iter().map(|c| c.text.clone()).collect()
}

/// 按 y 组行：同行 cell 的 y 差 <= `row_tol` 归一组，组内按 x 聚列。
/// `sorted` 须已按 y 升序。
fn group_rows(sorted: &[&Region], row_tol: f32, gap_thr: f32) -> Vec<Vec<TableCell>> {
    let mut rows = Vec::new();
    let mut cur: Vec<&Region> = Vec::new();
    let mut cur_y = 0.0_f32;
    for it in sorted {
        if !cur.is_empty() && (cur_y - it.y_min).abs() > row_tol {
            rows.push(cluster_row(&cur, gap_thr));
            cur.clear();
        }
        if cur.is_empty() {
            cur_y = it.y_min;
        }
        cur.push(it);
    }
    if !cur.is_empty() {
        rows.push(cluster_row(&cur, gap_thr));
    }
    rows
}

/// 行 y 中心（组内 cell 平均 y）。
fn row_center_y(r: &[TableCell]) -> f32 {
    if r.is_empty() {
        return 0.0;
    }
    r.iter().map(|c| c.y).sum::<f32>() / r.len() as f32
}

/// 自适应行距容差：0.5 × 中位行距，夹在「行高下限」与「0.3×页宽」之间。
/// 高 dpi 扫描件行距大，避免绝对 4px 把同行拆两行（C2）。
fn relative_row_tol(items: &[Region], page_w: f32) -> f32 {
    // 行高估计 = cell h 中位数（单行文本 h≈行高）。
    let mut hs: Vec<f32> = items.iter().map(|it| it.height()).collect();
    hs.sort_by(|a, b| a.total_cmp(b));
    let rh = if hs.is_empty() {
        10.0
    } else {
        hs[hs.len() / 2]
    };
    // 粗分组（容差 ~1.2×行高）估中位行距。
    let mut sorted: Vec<&Region> = items.iter().collect();
    sorted.sort_by(|a, b| a.y_min.total_cmp(&b.y_min));
    let coarse = group_rows(&sorted, (rh * 0.8).max(2.0), 0.012 * page_w);
    let ys: Vec<f32> = coarse.iter().map(|r| row_center_y(r)).collect();
    let mut pitch = 12.0_f32;
    if ys.len() >= 2 {
        let mut diffs: Vec<f32> = ys.windows(2).map(|w| (w[0] - w[1]).abs()).collect();
        diffs.sort_by(|a, b| a.total_cmp(b));
        pitch = diffs[diffs.len() / 2];
    }
    // 下限 4.0（原硬编码，保小尺度 PDF pt 行为）；上限随页宽，防超大容差误合并。
    // F4：`f32::clamp` 在 min>max / 任一 NaN 时 panic——页宽极小（<13.33）或 pitch 为
    // NaN（退化页行距全同）会触发。先滤 NaN 再手动 clamp（避免 panic 路径）。
    let upper = (0.3 * page_w).max(4.0);
    let tol = 0.5 * pitch;
    if tol.is_nan() || upper.is_nan() {
        return 4.0;
    }
    tol.clamp(4.0, upper)
}

/// 首行是否真表头（保守判定）。false-positive（数据行当表头）比 false-negative
/// 更坏：表头去重会删首数据行。规则：含表头关键词 / 文本显著短于数据行。
fn is_header_row(row: &[TableCell], body: &[Vec<TableCell>]) -> bool {
    const KW: &[&str] = &[
        "编号", "序号", "名称", "单位", "备注", "项目", "内容", "说明", "条款", "标准", "代号",
        "类别", "类型", "数量",
    ];
    let text: String = row.iter().map(|c| c.text.clone()).collect();
    if KW.iter().any(|k| text.contains(k)) {
        return true;
    }
    if body.is_empty() {
        return false;
    }
    let row_avg = row.iter().map(avg_len).sum::<f32>() / row.len().max(1) as f32;
    let body_avg = body.iter().flat_map(|r| r.iter()).map(avg_len).sum::<f32>()
        / body.iter().map(|r| r.len()).sum::<usize>().max(1) as f32;
    // 表头 cell 都很短（≤5 字）且数据明显更长 → 表头
    row_avg <= 5.0 && body_avg > row_avg * 1.8
}

/// cell 文本字符数（含空白）。
fn avg_len(c: &TableCell) -> f32 {
    c.text.chars().count() as f32
}

/// 跨页续接：列数一致时合并（若下页首行 == 已有表头 → 去重表头）。
/// 仅当 `acc` 首行是真表头才去重，否则纯数据表首行会被误删（C1）。
pub fn extend_table_grid(acc: &mut TableGrid, next: TableGrid) {
    if next.cols != acc.cols {
        return;
    }
    let first_matches = acc.has_header
        && next
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

/// 行内按 x 间隙聚列，返回 (首格 x, y, 高, 文本) 的单元格。
/// 先按 x 排序：OCR/提取块原始顺序不保证 x 序，乱序会聚出错误列（如倒序）。
fn cluster_row(items: &[&Region], gap_thr: f32) -> Vec<TableCell> {
    let mut items: Vec<&Region> = items.to_vec();
    items.sort_by(|a, b| a.x_min.total_cmp(&b.x_min));
    let mut cells: Vec<TableCell> = Vec::new();
    let mut cluster: Vec<&Region> = Vec::new();
    let mut x0 = 0.0_f32;
    let mut y0 = 0.0_f32;
    let mut hmax = 0.0_f32;
    for it in &items {
        let it = *it;
        if let Some(prev) = cluster.last()
            && it.x_min - prev.x_max > gap_thr
        {
            cells.push(TableCell {
                text: join_cell_items(&cluster),
                x: x0,
                y: y0,
                h: hmax,
            });
            cluster.clear();
        }
        if cluster.is_empty() {
            x0 = it.x_min;
            y0 = it.y_min;
            hmax = 0.0;
        }
        cluster.push(it);
        hmax = hmax.max(it.height());
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
fn join_cell_items(items: &[&Region]) -> String {
    let mut s = String::new();
    for (i, it) in items.iter().enumerate() {
        if i > 0 {
            let a = s
                .chars()
                .last()
                .map(|c| c.is_ascii_alphanumeric())
                .unwrap_or(false);
            let b = it
                .text
                .chars()
                .next()
                .map(|c| c.is_ascii_alphanumeric())
                .unwrap_or(false);
            if a && b {
                s.push(' ');
            }
        }
        s.push_str(it.text.trim());
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

/// HTML 转义：仅转义 `& < >`（当前调用方 attr 值均为 usize，无注入面）。
/// 安全约束：若未来把**用户输入文本**拼进属性值（如 `<td title="...">`），
/// 必须在此补转义 `"` 与 `'`，否则存在属性注入（XSS）风险（S1）。
fn escape_html(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 文本块构造：(x, y, w, h, text) → Region。
    fn blk(text: &str, x: f32, y: f32) -> Region {
        Region::from_top_left(x, y, 20.0, 10.0, text)
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

    /// 合并表头（表头行 x 与数据列不对齐）→ 仍重建（排除表头行的对齐检测）。
    #[test]
    fn merged_header_does_not_break_alignment() {
        // 表头 3 块（末块合并覆盖 c1+c2，x=40 偏离数据列 x=60）；数据行 4 列对齐
        let items = vec![
            blk("序号", 10.0, 10.0),
            blk("条款", 40.0, 10.0),
            blk("标准号+名称(合并表头)", 90.0, 10.0),
            blk("1", 10.0, 20.0),
            blk("3", 60.0, 20.0),
            blk("GJB1405", 120.0, 20.0),
            blk("装备质量管理术语", 180.0, 20.0),
            blk("2", 10.0, 30.0),
            blk("3", 60.0, 30.0),
            blk("CJB451", 120.0, 30.0),
            blk("可靠性术语", 180.0, 30.0),
            blk("3", 10.0, 40.0),
            blk("3", 60.0, 40.0),
            blk("GJB5000", 120.0, 40.0),
            blk("软件能力", 180.0, 40.0),
        ];
        let g = reconstruct_table_grid(&items, 300.0).expect("grid");
        assert_eq!(g.cols, 4);
        assert_eq!(g.rows.len(), 3);
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
                TableCell {
                    text: "ID".into(),
                    x: 10.0,
                    y: 10.0,
                    h: 10.0,
                },
                TableCell {
                    text: "Name".into(),
                    x: 60.0,
                    y: 10.0,
                    h: 10.0,
                },
            ],
            rows: vec![vec![
                TableCell {
                    text: "1".into(),
                    x: 10.0,
                    y: 20.0,
                    h: 10.0,
                },
                TableCell {
                    text: "Alice".into(),
                    x: 60.0,
                    y: 20.0,
                    h: 10.0,
                },
            ]],
            has_header: true,
        };
        // 下页：重复表头 + 续行
        let next = TableGrid {
            cols: 2,
            header: vec![
                TableCell {
                    text: "ID".into(),
                    x: 10.0,
                    y: 10.0,
                    h: 10.0,
                },
                TableCell {
                    text: "Name".into(),
                    x: 60.0,
                    y: 10.0,
                    h: 10.0,
                },
            ],
            rows: vec![
                vec![
                    TableCell {
                        text: "ID".into(),
                        x: 10.0,
                        y: 10.0,
                        h: 10.0,
                    },
                    TableCell {
                        text: "Name".into(),
                        x: 60.0,
                        y: 10.0,
                        h: 10.0,
                    },
                ],
                vec![
                    TableCell {
                        text: "2".into(),
                        x: 10.0,
                        y: 20.0,
                        h: 10.0,
                    },
                    TableCell {
                        text: "Bob".into(),
                        x: 60.0,
                        y: 20.0,
                        h: 10.0,
                    },
                ],
            ],
            has_header: true,
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
                TableCell {
                    text: "A".into(),
                    x: 10.0,
                    y: 10.0,
                    h: 10.0,
                },
                TableCell {
                    text: "B".into(),
                    x: 60.0,
                    y: 10.0,
                    h: 10.0,
                },
                TableCell {
                    text: "C".into(),
                    x: 110.0,
                    y: 10.0,
                    h: 10.0,
                },
            ],
            rows: vec![
                // 行0: x 高格(h=40) → rowspan 吞行1 c0；c1 空 → x colspan=2
                vec![
                    TableCell {
                        text: "x".into(),
                        x: 10.0,
                        y: 20.0,
                        h: 40.0,
                    },
                    TableCell {
                        text: "".into(),
                        x: 60.0,
                        y: 20.0,
                        h: 10.0,
                    },
                    TableCell {
                        text: "y".into(),
                        x: 110.0,
                        y: 20.0,
                        h: 10.0,
                    },
                ],
                vec![
                    TableCell {
                        text: "".into(),
                        x: 10.0,
                        y: 30.0,
                        h: 10.0,
                    },
                    TableCell {
                        text: "w".into(),
                        x: 60.0,
                        y: 30.0,
                        h: 10.0,
                    },
                    TableCell {
                        text: "".into(),
                        x: 110.0,
                        y: 30.0,
                        h: 10.0,
                    },
                ],
            ],
            has_header: true,
        };
        let html = table_grid_to_html(&g);
        assert!(html.contains("colspan=\"2\""), "期望 colspan，got: {html}");
        assert!(html.contains("rowspan=\"2\""), "期望 rowspan，got: {html}");
    }

    /// C1：纯数据表（无表头关键词、文本长）→ has_header=false；跨页续接
    /// 即使下页首行 == 表头首行也不去重，防丢首数据行。
    #[test]
    fn plain_data_table_preserves_first_row_across_pages() {
        let mut acc = TableGrid {
            cols: 2,
            header: vec![
                TableCell {
                    text: "张三".into(),
                    x: 10.0,
                    y: 10.0,
                    h: 10.0,
                },
                TableCell {
                    text: "北京市海淀区".into(),
                    x: 60.0,
                    y: 10.0,
                    h: 10.0,
                },
            ],
            rows: vec![vec![
                TableCell {
                    text: "李四".into(),
                    x: 10.0,
                    y: 20.0,
                    h: 10.0,
                },
                TableCell {
                    text: "上海市浦东".into(),
                    x: 60.0,
                    y: 20.0,
                    h: 10.0,
                },
            ]],
            has_header: false,
        };
        // 下页首行恰好 == acc 首行（纯数据巧合）——但因 has_header=false 不去重
        let next = TableGrid {
            cols: 2,
            header: vec![
                TableCell {
                    text: "张三".into(),
                    x: 10.0,
                    y: 10.0,
                    h: 10.0,
                },
                TableCell {
                    text: "北京市海淀区".into(),
                    x: 60.0,
                    y: 10.0,
                    h: 10.0,
                },
            ],
            rows: vec![
                vec![
                    TableCell {
                        text: "张三".into(),
                        x: 10.0,
                        y: 10.0,
                        h: 10.0,
                    },
                    TableCell {
                        text: "北京市海淀区".into(),
                        x: 60.0,
                        y: 10.0,
                        h: 10.0,
                    },
                ],
                vec![
                    TableCell {
                        text: "王五".into(),
                        x: 10.0,
                        y: 20.0,
                        h: 10.0,
                    },
                    TableCell {
                        text: "广州市天河".into(),
                        x: 60.0,
                        y: 20.0,
                        h: 10.0,
                    },
                ],
            ],
            has_header: false,
        };
        extend_table_grid(&mut acc, next);
        assert_eq!(acc.rows.len(), 3, "纯数据表跨页不去重首行（防丢行）");
    }

    /// C2：高 dpi 扫描件（行距 ~60px，同列 y 抖动 ~±8px）→ 旧绝对 4px 会把
    /// 同行拆两行；相对 row_tol（0.5×行距≈30px）正确分 3 行而非 6 行。
    #[test]
    fn high_dpi_jitter_rows_still_grouped() {
        // 高 dpi OCR：cell 高 ~25（非 blk 默认 10），行距 ~60px，同列 y 抖动 ±8px
        // （>旧绝对 4px 会误拆行）。相对 row_tol（0.5×行距≈30px）正确分 3 行。
        let mut items = Vec::new();
        let rows_y = [10.0_f32, 70.0, 130.0, 190.0];
        for (i, &y) in rows_y.iter().enumerate() {
            let jit = if i % 2 == 0 { 8.0 } else { -6.0 };
            items.push(Region::from_top_left(
                10.0,
                y + jit,
                20.0,
                25.0,
                format!("r{i}a"),
            ));
            items.push(Region::from_top_left(
                80.0,
                y - jit,
                20.0,
                25.0,
                format!("r{i}b"),
            ));
            items.push(Region::from_top_left(
                150.0,
                y + jit * 0.5,
                20.0,
                25.0,
                format!("r{i}c"),
            ));
        }
        let g = reconstruct_table_grid(&items, 300.0).expect("grid");
        assert_eq!(g.cols, 3);
        assert_eq!(g.rows.len(), 3, "抖动行仍正确分 3 行（非 6 行）");
    }
}
