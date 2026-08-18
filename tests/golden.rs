//! 端到端 golden 回归网（T01）
//!
//! 对每个样本跑 `convert_to_markdown`，输出 SHA-256 与 `tests/golden/snapshots/<name>.sha256`
//! 比对；不一致即失败（打印 hash 差异）。保护后续重构（T02-T10）行为不变。
//!
//! 用法：
//! - 默认 `cargo test --test golden` —— 只跑非 OCR 样本（不触发模型下载）。
//! - `ANYDOC_GOLDEN_OCR=1` —— 追加跑 OCR 样本（image.* / 真实扫描件）。
//! - `ANYDOC_GOLDEN_UPDATE=1` —— 重生成快照（仅行为变更 ticket 才改基线）。
//! - 样本文件缺失 → 跳过（如 CI 无 gitignored 的 real_samples）。
//! - 样本存在但快照缺失且非 update → 失败，强制显式 `UPDATE=1` 建基线。

use std::collections::hash_map::DefaultHasher;
use std::hash::Hasher;
use std::path::PathBuf;

use anydoc_ocr::{ConvertRequest, ForceFlags, ParallelConfig, RenderConfig, convert_to_markdown};

/// (相对 CARGO_MANIFEST_DIR 的样本路径, 是否需 OCR 引擎)
fn samples() -> Vec<(&'static str, bool)> {
    vec![
        // 已入库生成样本（小、确定）：默认跑非 OCR
        ("tests/samples/text.pdf", false),
        ("tests/samples/text.ofd", false),
        ("tests/samples/text_font.ofd", false),
        ("tests/samples/multipage.pdf", false),
        ("tests/samples/real_table.pdf", false),
        // OCR 类（需 env）
        ("tests/samples/image.pdf", true),
        ("tests/samples/image.ofd", true),
        ("tests/samples/image_table.pdf", true),
        ("tests/samples/image_table.ofd", true),
        // 真实样本（gitignored）：需 OCR env 才跑，本地存在才生成快照
        ("tests/real_samples/gwy_ling825_xingzhengzhifa.pdf", true),
        ("tests/real_samples/gwy_gongbao2026_01.pdf", true),
        ("tests/real_samples/shehui_xinyong_tixi.pdf", true),
        ("tests/real_samples/zirenhuanjing_tudiliuyu.pdf", true),
        ("tests/real_samples/上海公报2025第1期.pdf", true),
        ("tests/real_samples/北京公报2026第8期.pdf", true),
        ("tests/real_samples/meili_zhongguo_15_5.pdf", true),
        ("tests/real_samples/longhai_gb2021_1.pdf", true),
        ("tests/real_samples/intro_suwell.ofd", true),
        ("tests/real_samples/gov_taiyuan_gongbao_2025_02.ofd", true),
        ("tests/real_samples/gov_taiyuan_gongbao_2025_03.ofd", true),
        ("tests/real_samples/crosspage_table.pdf", true),
        ("tests/real_samples/GJB9001C-2017质量管理体系要求.pdf", true),
    ]
}

fn snapshot_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/golden/snapshots")
}

fn snap_name(rel: &str) -> String {
    rel.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '_'
            }
        })
        .collect()
}

fn hash_of(s: &str) -> String {
    let mut h = DefaultHasher::new();
    h.write(s.as_bytes());
    format!("{:016x}", h.finish())
}

#[test]
fn golden_outputs_are_stable() {
    let update = std::env::var("ANYDOC_GOLDEN_UPDATE").is_ok();
    let with_ocr = std::env::var("ANYDOC_GOLDEN_OCR").is_ok();
    let dir = snapshot_dir();
    std::fs::create_dir_all(&dir).expect("mkdir snapshots");

    // P2 后 ConvertRequest::default 的 dpi 已是 100.0（旧 dpi=0 陷阱已修）；
    // 此处仍显式设以钉死 golden 基线的渲染参数
    let opts = ConvertRequest {
        render: RenderConfig { dpi: 100.0 },
        parallel: ParallelConfig { page_parallel: 4, ..Default::default() },
        ..Default::default()
    };

    let mut failures = Vec::new();
    let mut skipped = Vec::new();
    let mut checked = 0usize;

    for (rel, needs_ocr) in samples() {
        if needs_ocr && !with_ocr {
            skipped.push(format!("{rel} (OCR, skip)"));
            continue;
        }
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel);
        if !path.exists() {
            skipped.push(format!("{rel} (missing)"));
            continue;
        }
        let md = match convert_to_markdown(&path, &opts, ForceFlags::default()) {
            Ok(m) => m,
            Err(e) => {
                failures.push(format!("{rel}: convert error: {e}"));
                continue;
            }
        };
        let got = hash_of(&md);
        let snap = dir.join(format!("{}.sha256", snap_name(rel)));
        if update {
            std::fs::write(&snap, &got).expect("write snapshot");
            println!("[golden] update {rel} -> {got}");
            checked += 1;
            continue;
        }
        match std::fs::read_to_string(&snap) {
            Ok(want) if want.trim() == got => {
                checked += 1;
            }
            Ok(want) => failures.push(format!(
                "{rel}: hash mismatch\n  want {}\n  got  {}\n  (output len {})",
                want.trim(),
                got,
                md.len()
            )),
            Err(_) => failures.push(format!(
                "{rel}: missing baseline snapshot; run with ANYDOC_GOLDEN_UPDATE=1 (output len {})",
                md.len()
            )),
        }
    }

    for s in &skipped {
        println!("[golden] skip {s}");
    }
    if !failures.is_empty() {
        eprintln!(
            "[golden] {} failure(s), {} checked, {} skipped:",
            failures.len(),
            checked,
            skipped.len()
        );
        for f in &failures {
            eprintln!("[golden] FAIL {f}");
        }
        panic!("golden regression: {}", failures.len());
    }
    println!("[golden] OK: {checked} checked, {} skipped", skipped.len());
}
