//! `--pages` 页码范围语法（借鉴 MinerU 4.0 → docvortex `document/page_range.py`，
//! 本仓直接读得其源码，语义逐条对齐）：
//!
//! - 逗号分隔段，每段 `N` / `A-B`（1 基、含端点）/ `rN`（从末页倒数，`r1` = 末页）；
//! - `all` 或空白 = 全选（不裁剪）；
//! - 结果排序去重，越界端点裁剪；裁剪后无交集 → 显式报错（不静默空产出）；
//! - 倒序区间（两端同号且 start>end）非法；`~`、前导零、负数、`5-` 开放端一律非法
//!   （docvortex `_SEGMENT_PATTERN` 只接受 `r?[1-9][0-9]*`）；
//! - 页数为 0 的文档 → 报错（docvortex "document has no available pages"）。
//!
//! 错误口径：`Unsupported`（用户显式输入非法，与 dpi 闸同风格——报错比按错值
//! 跑完便宜），detail 点名语法。仅 PDF 通道消费（MinerU 对非 PDF 报
//! `page_range_invalid`，我们同样在调度层拒绝）。

use std::collections::BTreeSet;
use std::path::Path;

use crate::error::{ConvertError, ErrorKind, Result, Stage};

/// 段端点：正数 = 1 基正序；负数 = 从末页倒数（`r1` → -1），求值期换算。
type Segment = (i64, i64);

/// 空白或 `all` = 不设限（与 docvortex `normalize_page_range_input` 同语义：
/// 显式 `all` 与未提供等效，`--pages all` 不触发非 PDF 拒绝）。
pub(crate) fn is_unrestricted(raw: &str) -> bool {
    let t = raw.trim();
    t.is_empty() || t.eq_ignore_ascii_case("all")
}

/// 语法错误（对齐 docvortex `_invalid_range` 的三类 reason，文案中文）。
fn invalid(raw: &str, reason: &str) -> ConvertError {
    ConvertError::new(
        ErrorKind::Unsupported,
        Stage::Convert,
        format!(
            "非法页码范围 {raw:?}: {reason}（语法: 1-5,8,r3-r1 或 all；1 基、含端点，rN 从末页倒数）"
        ),
    )
}

/// 解析单个端点 token（docvortex `_parse_endpoint` + 正则约束）：
/// `r?[1-9][0-9]*`，`r` 前缀取负；溢出报"页码数字过大"。
fn endpoint(token: &str, raw: &str) -> Result<i64> {
    let (rev, digits) = match token.strip_prefix('r') {
        Some(d) => (true, d),
        None => (false, token),
    };
    let ok = !digits.is_empty()
        && digits.bytes().enumerate().all(|(i, b)| match b {
            b'1'..=b'9' => true,
            b'0' => i > 0,
            _ => false,
        });
    if !ok {
        return Err(invalid(raw, "页码须为正整数（rN 表示倒数）"));
    }
    let n: i64 = digits
        .parse()
        .map_err(|_| invalid(raw, "页码数字过大"))?;
    Ok(if rev { -n } else { n })
}

/// 解析完整表达式为段列表（docvortex `_parse_segments`）；不设限 → `None`。
fn parse(raw: &str) -> Result<Option<Vec<Segment>>> {
    let value = raw.trim();
    if value.is_empty() || value.eq_ignore_ascii_case("all") {
        return Ok(None);
    }
    let mut segments: Vec<Segment> = Vec::new();
    for part in value.split(',') {
        let t = part.trim();
        match t.split_once('-') {
            None => {
                let s = endpoint(t, raw)?;
                segments.push((s, s));
            }
            Some((a, b)) => {
                let s = endpoint(a.trim(), raw)?;
                let e = endpoint(b.trim(), raw)?;
                // 两端同为正序或同为倒序时拒绝 start>end（docvortex 同判据；
                // 异号端点要等知道总页数才能比较，留到 resolve）
                if (s > 0) == (e > 0) && s > e {
                    return Err(invalid(raw, "不支持倒序区间"));
                }
                segments.push((s, e));
            }
        }
    }
    Ok(Some(segments))
}

/// 求值（docvortex `_resolved_intervals` + `parse_page_range`）：rN 换算 →
/// 倒序复验 → 裁剪越界 → 合并相邻/重叠区间 → 展开为 1 基升序页号集合。
/// 与文档页范围无交集 / 页数 0 → 显式 Err。
pub(crate) fn resolve(raw: &str, page_count: u32) -> Result<BTreeSet<u32>> {
    let count = page_count as i64;
    let segments = match parse(raw)? {
        None if page_count == 0 => return Err(invalid(raw, "文档无可处理页")),
        None => return Ok((1..=page_count).collect()),
        Some(s) => s,
    };
    if page_count == 0 {
        return Err(invalid(raw, "文档无可处理页"));
    }
    let mut intervals: Vec<(i64, i64)> = Vec::new();
    for (s, e) in segments {
        let s2 = if s > 0 { s } else { count + s + 1 };
        let e2 = if e > 0 { e } else { count + e + 1 };
        if s2 > e2 {
            return Err(invalid(raw, "不支持倒序区间"));
        }
        let (lo, hi) = (1i64.max(s2), count.min(e2));
        if lo <= hi {
            intervals.push((lo, hi));
        }
    }
    if intervals.is_empty() {
        return Err(invalid(raw, &format!("选页为空（文档共 {page_count} 页，无交集）")));
    }
    intervals.sort_unstable();
    let mut merged: Vec<(i64, i64)> = Vec::new();
    for (s, e) in intervals {
        match merged.last_mut() {
            Some(last) if s <= last.1 + 1 => last.1 = last.1.max(e),
            _ => merged.push((s, e)),
        }
    }
    Ok(merged
        .iter()
        .flat_map(|(s, e)| *s as u32..=*e as u32)
        .collect())
}

/// `--pages` 入口：不设限（空/`all`）→ `Ok(None)`；否则解析+求值为页号集合。
pub(crate) fn select(raw: Option<&str>, page_count: u32) -> Result<Option<BTreeSet<u32>>> {
    match raw {
        Some(r) if !is_unrestricted(r) => resolve(r, page_count).map(Some),
        _ => Ok(None),
    }
}

/// 纯语法预检（不依赖页数）：供 CLI 早失败，省掉 stdin 落盘等前置开销。
/// 语义合法但需页数判定的（rN、空集）在这里恒通过，由 [`resolve`] 终审。
pub(crate) fn check_syntax(raw: &str) -> Result<()> {
    parse(raw).map(|_| ())
}

/// 非 PDF 文档给了页码选择 → 显式拒绝（MinerU 对非 PDF 报 `page_range_invalid`）。
pub(crate) fn reject_non_pdf<P: AsRef<Path>>(path: P, raw: &str) -> ConvertError {
    ConvertError::new(
        ErrorKind::Unsupported,
        Stage::Convert,
        format!(
            "页码选择(--pages {raw:?})仅支持 PDF，输入不是 PDF: {}",
            path.as_ref().display()
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(raw: &str, count: u32) -> Vec<u32> {
        resolve(raw, count).unwrap().into_iter().collect()
    }

    #[test]
    fn unrestricted_values() {
        assert!(is_unrestricted(""));
        assert!(is_unrestricted("  "));
        assert!(is_unrestricted("all"));
        assert!(is_unrestricted("ALL"));
        assert!(!is_unrestricted("1-3"));
        // all/空白 → 全选等价
        assert_eq!(r("all", 4), vec![1, 2, 3, 4]);
        assert_eq!(r("  ", 2), vec![1, 2]);
    }

    #[test]
    fn basic_ranges_sorted_deduped_clipped() {
        // 乱序、重叠、越界（docvortex：排序去重、越界裁剪不报错）
        assert_eq!(r("8,3-5,1,4", 8), vec![1, 3, 4, 5, 8]);
        assert_eq!(r("5-9", 6), vec![5, 6]);
        // 相邻区间合并后仍逐页展开
        assert_eq!(r("1-2,3-4", 5), vec![1, 2, 3, 4]);
    }

    #[test]
    fn reverse_notation() {
        // r1 = 末页；r3-r1 = 末三页
        assert_eq!(r("r1", 10), vec![10]);
        assert_eq!(r("r3-r1", 10), vec![8, 9, 10]);
        // 混合端点 5-r2（6 页文档 → 5..=5）
        assert_eq!(r("5-r2", 6), vec![5]);
        // r 端点越界裁剪
        assert_eq!(r("r20-r1", 3), vec![1, 2, 3]);
    }

    #[test]
    fn invalid_syntax_rejected() {
        // 倒序区间
        assert!(resolve("5-2", 8).is_err());
        assert!(resolve("r2-r5", 8).is_err());
        // 求值后倒序（5-r2 于 4 页文档：5 > 3）
        let e = resolve("5-r2", 4).unwrap_err();
        assert_eq!(e.kind, ErrorKind::Unsupported);
        // 非法 token：0、前导零、负数、开放端、~、空段、非数字
        for bad in ["0", "1-0", "-3", "5-", "-5", "1~3", "1,,2", "x", "1-2-3", "r0"] {
            assert!(resolve(bad, 8).is_err(), "应非法: {bad}");
        }
        // 溢出
        assert!(resolve("99999999999999999999", 8).is_err());
    }

    #[test]
    fn empty_intersection_and_zero_pages_error() {
        // 越界致空集：显式报错、不静默空产出
        assert!(resolve("12", 10).is_err());
        assert!(resolve("8-12", 4).is_err());
        assert!(resolve("1-3", 0).is_err());
        assert!(resolve("all", 0).is_err());
    }

    #[test]
    fn select_entry_semantics() {
        assert_eq!(select(None, 5).unwrap(), None);
        assert_eq!(select(Some("all"), 5).unwrap(), None);
        assert_eq!(select(Some(""), 5).unwrap(), None);
        assert_eq!(
            select(Some("2,4"), 5).unwrap(),
            Some([2u32, 4].into_iter().collect::<BTreeSet<_>>())
        );
        assert!(select(Some("4-2"), 5).is_err());
    }
}
