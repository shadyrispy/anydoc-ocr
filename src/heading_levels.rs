//! 标题分级（布局驱动，`ANYDOC_HEADINGS_LAYOUT`）：镜像上游
//! `oar-ocr-core` 的 `infer_paragraph_title_levels` 三信号投票。
//!
//! 现网默认路径只有**编号语义**一条信号：
//! - 文字层（PDF/OFD）：`title_levels(lines, &[], numbering=true)`——
//!   `一、`/`1.1` 之类能分级，**无编号的"总则""适用范围"整类标题检不出**；
//! - OCR 路（gfm）：版面已给出 title 块，但级别仍走编号启发式，
//!   无编号时统一回落 `##`（一篇文档里一级/二级/三级标题被抹平为同级）。
//!
//! 本模块补上 MinerU/上游的另两条**布局**信号，与语义信号做加权投票：
//!   语义（编号）权重 2 > 行高 k-means 权重 1 = 缩进 k-means 权重 1。
//!
//! 纯函数、可完整单测；调用方负责挑候选与取几何。开关语义沿本仓既有族
//! （变量存在即开启、默认关闭），默认输出逐字节不变。

use std::collections::HashMap;

/// 语义信号（编号/关键词）在投票里的权重（上游 `score[level] += 2`）。
pub const SEMANTIC_WEIGHT: u8 = 2;
/// 布局信号（行高、缩进）各自的权重（上游各 `+= 1`）。
pub const LAYOUT_WEIGHT: u8 = 1;

/// markdown 标题级别区间（上游 `clamp(2, 6)`，本模块保留 `1` 给文档级标题）。
pub const LEVEL_MIN: usize = 1;
pub const LEVEL_MAX: usize = 6;

/// 把一维标量特征聚成层级（1D k-means，镜像上游 `infer_levels_by_kmeans_feature`）。
///
/// - `descending = true`：特征值越大级别越高（行高——字越大越像一级标题）；
/// - `descending = false`：特征值越小级别越高（缩进——越靠左越高）。
///
/// 与上游逐条对齐（保持一致性优先于自创）：
/// 1. 少于 2 个有效样本 → 空结果（信号缺失，不参与投票）；
/// 2. 类数 k = 不同值数（差 >1e-3 视为不同）夹到 `[1,4]`，且不超过样本数；
///    k<=1 → 空结果（全页同一字号时不该硬分层级）；
/// 3. 种子取排序后各分位段的中心点（确定性，无随机）；
/// 4. 16 轮 Lloyd 迭代（空类质心保留上一轮，故类序稳定 → 类序即级别序）。
///
/// 返回 `样本下标 -> 级别`（级别从 **1** 起，按特征方向排序后的类序）。
pub fn cluster_levels(samples: &[(usize, f32)], descending: bool) -> HashMap<usize, usize> {
    let clean: Vec<(usize, f32)> = samples
        .iter()
        .copied()
        .filter(|(_, v)| v.is_finite())
        .collect();
    if clean.len() < 2 {
        return HashMap::new();
    }
    let mut values: Vec<f32> = clean.iter().map(|(_, v)| *v).collect();
    values.sort_by(|a, b| a.total_cmp(b));
    let unique = values.windows(2).filter(|w| (w[1] - w[0]).abs() > 1e-3).count() + 1;
    let k = unique.clamp(1, 4).min(clean.len());
    if k <= 1 {
        return HashMap::new();
    }
    let mut centroids: Vec<f32> = (0..k)
        .map(|i| {
            let pos = ((i as f32 + 0.5) / k as f32 * values.len() as f32).floor() as usize;
            values[pos.min(values.len() - 1)]
        })
        .collect();
    for _ in 0..16 {
        let mut sums = vec![0.0f32; k];
        let mut counts = vec![0usize; k];
        for (_, v) in &clean {
            let (mut bi, mut bd) = (0usize, f32::INFINITY);
            for (idx, c) in centroids.iter().enumerate() {
                let d = (v - c).abs();
                if d < bd {
                    bd = d;
                    bi = idx;
                }
            }
            sums[bi] += *v;
            counts[bi] += 1;
        }
        for idx in 0..k {
            if counts[idx] > 0 {
                centroids[idx] = sums[idx] / counts[idx] as f32;
            }
        }
    }
    // 质心升序 → 类序；descending 时反向（大特征 = 高级别 = 小 level）
    let mut order: Vec<usize> = (0..k).collect();
    order.sort_by(|&a, &b| centroids[a].total_cmp(&centroids[b]));
    if descending {
        order.reverse();
    }
    let mut rank = vec![0usize; k];
    for (r, &ci) in order.iter().enumerate() {
        rank[ci] = r + 1;
    }
    let mut out = HashMap::new();
    for (idx, v) in &clean {
        let (mut bi, mut bd) = (0usize, f32::INFINITY);
        for (i, c) in centroids.iter().enumerate() {
            let d = (v - c).abs();
            if d < bd {
                bd = d;
                bi = i;
            }
        }
        out.insert(*idx, rank[bi]);
    }
    out
}

/// 三信号加权投票出标题级别（镜像上游 voted 分支）。
///
/// 同分时**语义优先**，再取更小级别（上游：`is_semantic && !best_is_semantic`
/// 或同级平手取更小 level）。三信号全缺 → `fallback`（调用方给的历史默认）。
pub fn vote_level(
    semantic: Option<usize>,
    font: Option<usize>,
    indent: Option<usize>,
    fallback: usize,
) -> usize {
    let mut score = [0u8; LEVEL_MAX + 1];
    if let Some(l) = semantic {
        score[l.clamp(LEVEL_MIN, LEVEL_MAX)] += SEMANTIC_WEIGHT;
    }
    if let Some(l) = font {
        score[l.clamp(LEVEL_MIN, LEVEL_MAX)] += LAYOUT_WEIGHT;
    }
    if let Some(l) = indent {
        score[l.clamp(LEVEL_MIN, LEVEL_MAX)] += LAYOUT_WEIGHT;
    }
    let mut best = semantic.or(font).or(indent).unwrap_or(fallback).clamp(LEVEL_MIN, LEVEL_MAX);
    let mut best_score = 0u8;
    for level in LEVEL_MIN..=LEVEL_MAX {
        let s = score[level];
        if s > best_score {
            best_score = s;
            best = level;
        } else if s == best_score && s > 0 {
            let is_sem = semantic == Some(level);
            let best_is_sem = semantic == Some(best);
            if (is_sem && !best_is_sem) || (is_sem == best_is_sem && level < best) {
                best = level;
            }
        }
    }
    if best_score == 0 {
        return semantic.or(font).or(indent).unwrap_or(fallback).clamp(LEVEL_MIN, LEVEL_MAX);
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 行高两档（24/16pt）→ 两簇，大字号 = level 1（descending=true）。
    #[test]
    fn height_clusters_two_levels() {
        let s = vec![(0usize, 24.0), (1, 24.0), (2, 16.0), (3, 16.0), (4, 16.0)];
        let g = cluster_levels(&s, true);
        assert_eq!(g.len(), 5);
        assert_eq!(g[&0], 1, "大字 = 一级");
        assert_eq!(g[&2], 2, "小字 = 二级");
        assert_eq!(g[&4], 2);
    }

    /// 缩进越靠左级别越高（descending=false）。
    #[test]
    fn indent_clusters_higher_level_when_left() {
        let s = vec![(0usize, 40.0), (1, 40.0), (2, 120.0), (3, 120.0)];
        let g = cluster_levels(&s, false);
        assert_eq!(g[&0], 1);
        assert_eq!(g[&2], 2);
    }

    /// 全页同一特征值（无差异）→ 不硬分层级（k<=1 → 空）。
    #[test]
    fn identical_feature_yields_no_levels() {
        let s = vec![(0usize, 20.0), (1, 20.0), (2, 20.0)];
        assert!(cluster_levels(&s, true).is_empty());
    }

    #[test]
    fn fewer_than_two_samples_yields_no_levels() {
        assert!(cluster_levels(&[(0, 24.0)], true).is_empty());
        // 非有限值不参与：只剩 1 个有效样本 → 空
        let s = vec![(0usize, 24.0), (1, f32::NAN), (2, f32::INFINITY)];
        assert!(cluster_levels(&s, true).is_empty());
        assert!(cluster_levels(&[], false).is_empty());
    }

    /// 类数上限 4：字号连续分 6 档也只分 4 级（上游 k 夹到 4）。
    #[test]
    fn clusters_cap_at_four_levels() {
        let s: Vec<(usize, f32)> = (0..12).map(|i| (i, 12.0 + i as f32 * 6.0)).collect();
        let g = cluster_levels(&s, true);
        assert!(g.values().all(|&l| (1..=4).contains(&l)), "{g:?}");
        assert!(g.values().any(|&l| l == 1));
        assert!(g.values().any(|&l| l == 4), "12 个明显不同的字号应吃满 4 级");
        // 最大字号 = 1 级，最小 = 4 级
        assert_eq!(g[&11], 1);
        assert_eq!(g[&0], 4);
    }

    #[test]
    fn semantic_outweighs_a_single_layout_signal() {
        // 语义说 3 级、行高说 1 级：语义权重 2 > 布局 1 → 3
        assert_eq!(vote_level(Some(3), Some(1), None, 2), 3);
        // 语义 + 行高一致 → 更强
        assert_eq!(vote_level(Some(3), Some(3), Some(2), 2), 3);
    }

    /// 平手取语义、再取更小级别（上游 tie-break 逐条一致）。
    #[test]
    fn ties_prefer_semantic_then_smaller_level() {
        // font=1(1) vs indent=2(1)：平手，无语义 → 取更小 level
        assert_eq!(vote_level(None, Some(1), Some(2), 2), 1);
        // semantic=2(2) vs font=1(1)+indent=1(1)? indent 也是 1 → 平手 2:2 → 语义优先
        assert_eq!(vote_level(Some(2), Some(1), Some(1), 2), 2);
    }

    #[test]
    fn all_missing_falls_back() {
        assert_eq!(vote_level(None, None, None, 2), 2);
        assert_eq!(vote_level(None, None, None, 5), 5);
        // 越界钳制
        assert_eq!(vote_level(Some(9), None, None, 2), LEVEL_MAX);
        assert_eq!(vote_level(Some(0), None, None, 2), LEVEL_MIN);
    }
}
