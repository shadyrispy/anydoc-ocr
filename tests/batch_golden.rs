//! 批处理 golden（ADR-0005 候选 2）：验证跨文档 pipeline 的 (doc_idx, page_idx)
//! 分组正确性——多个图片型 PDF 一次性送入 `BatchConverter::convert_many`，
//! 每文档输出应与单文档 `convert_to_markdown` 输出**完全一致**（同 hash）。
//!
//! 防回归点：复合键 doc_idx 错位、GFM 跨文档串档、pipeline 文档边界丢失页等。
//!
//! 用法：
//! - `cargo test --test batch_golden` 默认跑非 OCR 样本（text.ofd/text.pdf + 文字层快速路径）
//! - `ANYDOC_GOLDEN_OCR=1` 追加图片型样本（image.pdf/image_table.pdf 跨文档 OCR）
//! - `ANYDOC_GOLDEN_UPDATE=1` 重生成快照

use std::collections::hash_map::DefaultHasher;
use std::hash::Hasher;
use std::path::PathBuf;

use anydoc_ocr::{ConvertOptions, ForceFlags, batch::BatchConverter, convert_to_markdown};

/// (相对 CARGO_MANIFEST_DIR 的样本路径, 是否需 OCR)
fn samples() -> Vec<(&'static str, bool)> {
    vec![
        // 文字型快速路径样本（默认跑）
        ("tests/samples/text.pdf", false),
        ("tests/samples/text.ofd", false),
        ("tests/samples/multipage.pdf", false),
        ("tests/samples/real_table.pdf", false),
        // 图片型样本（OCR env 才跑）—— 跨文档 OCR 分组的关键验证
        ("tests/samples/image.pdf", true),
        ("tests/samples/image_table.pdf", true),
    ]
}

fn snapshot_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/golden/snapshots")
}

fn snap_name(rel: &str) -> String {
    format!(
        "batch_{}",
        rel.chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                    c
                } else {
                    '_'
                }
            })
            .collect::<String>()
    )
}

fn hash_of(s: &str) -> String {
    let mut h = DefaultHasher::new();
    h.write(s.as_bytes());
    format!("{:016x}", h.finish())
}

#[test]
fn batch_matches_single_doc_output() {
    // 验证策略：每个样本先单文档跑 convert_to_markdown 拿基准 hash；
    // 再把所有样本一次性送 BatchConverter::convert_many，每文档输出 hash 应与基准一致。
    // 不一致即跨文档 pipeline 串档或丢页。
    let update = std::env::var("ANYDOC_GOLDEN_UPDATE").is_ok();
    let with_ocr = std::env::var("ANYDOC_GOLDEN_OCR").is_ok();
    let dir = snapshot_dir();
    std::fs::create_dir_all(&dir).expect("mkdir snapshots");

    let opts = ConvertOptions {
        dpi: 100.0,
        threads: 4,
        ..Default::default()
    };

    // 收集可用样本
    let mut paths: Vec<PathBuf> = Vec::new();
    let mut skipped = Vec::new();
    for (rel, needs_ocr) in samples() {
        if needs_ocr && !with_ocr {
            skipped.push(format!("{rel} (OCR, skip)"));
            continue;
        }
        let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel);
        if !p.exists() {
            skipped.push(format!("{rel} (missing)"));
            continue;
        }
        paths.push(p);
    }

    if paths.is_empty() {
        println!("[batch_golden] all samples skipped: {:?}", skipped);
        return;
    }

    // 单文档基准
    let mut single_hashes: Vec<String> = Vec::with_capacity(paths.len());
    for p in &paths {
        let md = match convert_to_markdown(p, &opts, ForceFlags::default()) {
            Ok(m) => m,
            Err(e) => panic!("[batch_golden] 单文档 {} 转换失败: {e}", p.display()),
        };
        single_hashes.push(hash_of(&md));
    }

    // 批处理：所有样本一次性送入 BatchConverter
    let converter = BatchConverter::new(opts.clone(), ForceFlags::default());
    let results = converter.convert_many(&paths);
    assert_eq!(
        results.len(),
        paths.len(),
        "[batch_golden] 批处理结果数 {} != 输入文档数 {}",
        results.len(),
        paths.len()
    );

    let mut failures = Vec::new();
    let mut checked = 0usize;
    for (i, (path, result)) in paths.iter().zip(results).enumerate() {
        let md = match result {
            Ok(m) => m,
            Err(e) => {
                failures.push(format!(
                    "{}: 批处理失败: {e}",
                    path.display()
                ));
                continue;
            }
        };
        let batch_hash = hash_of(&md);

        // 关键断言：批处理输出 hash == 单文档输出 hash
        if batch_hash != single_hashes[i] {
            failures.push(format!(
                "{}: 批处理输出与单文档不一致\n  single {}\n  batch  {}\n  (len single={}, batch={})",
                path.display(),
                single_hashes[i],
                batch_hash,
                0, // len 已在 hash 之外记不到，置 0
                md.len()
            ));
            continue;
        }

        // 与磁盘快照对比（保证跨 commit 稳定）
        let snap = dir.join(format!("{}.sha256", snap_name(path.file_name().unwrap().to_str().unwrap())));
        if update {
            std::fs::write(&snap, &batch_hash).expect("write snapshot");
            println!("[batch_golden] update {} -> {}", path.display(), batch_hash);
            checked += 1;
            continue;
        }
        match std::fs::read_to_string(&snap) {
            Ok(want) if want.trim() == batch_hash => checked += 1,
            Ok(want) => failures.push(format!(
                "{}: 快照漂移\n  want {}\n  got  {}",
                path.display(),
                want.trim(),
                batch_hash
            )),
            Err(_) => {
                // 快照缺失——首次建基线不算失败，告警即可（避免阻塞首次 commit）
                println!(
                    "[batch_golden] {} 无快照，hash={batch_hash}（ANYDOC_GOLDEN_UPDATE=1 建基线）",
                    path.display()
                );
                checked += 1;
            }
        }
    }

    for s in &skipped {
        println!("[batch_golden] skip {s}");
    }
    if !failures.is_empty() {
        eprintln!(
            "[batch_golden] {} failure(s), {} checked, {} skipped:",
            failures.len(),
            checked,
            skipped.len()
        );
        for f in &failures {
            eprintln!("[batch_golden] FAIL {f}");
        }
        panic!("batch golden regression: {}", failures.len());
    }
    println!(
        "[batch_golden] OK: {checked} checked, {} skipped",
        skipped.len()
    );
}

/// 失败路径回归（ADR-0005 错误隔离 + brooks-review Critical 修复）：
/// 混入一个"伪 PDF"（含 `%PDF` 魔数但内容损坏，detect 判定为 Pdf 但 pdfium 加载失败），
/// 断言该槽位是 `Err` 而非 `Ok(空串)`，且不影响同批其他有效文档的成功产出。
///
/// 防回归：[batch.rs] 的 `Ok(md_per_doc)` 兜底分支曾误把"doc 打开失败被
/// render_cross_doc_fn 跳过"标为 `Ok(String::new())`，导致 main.rs 写出空 .md
/// 并计入成功。修复后该槽位必须走 `Err` 通道。
#[test]
fn batch_isolates_corrupt_pdf_as_err() {
    use std::io::Write;

    let opts = ConvertOptions {
        dpi: 100.0,
        threads: 4,
        ..Default::default()
    };
    // force_ocr 让伪 PDF 直接进 ocr_paths，绕过 text_layer 预分流
    // （text_layer_markdown 对损坏 PDF 会返回 Ok(None) 也会进 ocr_paths，
    //  但 force_ocr 路径更直接、不依赖 pdf-inspector 的容错行为）
    let force = ForceFlags {
        pdf_force_ocr: true,
        ..Default::default()
    };

    // 有效样本：multipage.pdf 是文字型 PDF，force_ocr 下走 OCR pipeline 应成功
    let valid = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/samples/multipage.pdf");
    if !valid.exists() {
        println!("[batch_fail] skip: tests/samples/multipage.pdf missing");
        return;
    }

    // 伪 PDF：写入 %PDF 魔数 + 垃圾内容。detect 看到 %PDF 头判 DocKind::Pdf，
    // 但 pdfium.load_pdf_from_file 会失败 → render_cross_doc_fn 跳过该 doc →
    // convert_pdf_ocr 返回 Ok 但缺该 doc_idx → batch.rs 兜底标 Err。
    let bogus_dir = std::env::temp_dir().join("anydoc_batch_fail_test");
    std::fs::create_dir_all(&bogus_dir).expect("mkdir temp");
    let bogus = bogus_dir.join("corrupt.pdf");
    let mut f = std::fs::File::create(&bogus).expect("create corrupt.pdf");
    f.write_all(b"%PDF-1.4\nthis is not a real pdf body\n%%EOF\n").expect("write");
    drop(f);

    let paths = vec![valid.clone(), bogus.clone()];
    let converter = BatchConverter::new(opts, force);
    let results = converter.convert_many(&paths);

    assert_eq!(
        results.len(),
        2,
        "[batch_fail] 结果数应等于输入文档数"
    );

    // 第一个（有效 multipage.pdf）应成功——错误隔离：单文档失败不炸整批
    assert!(
        results[0].is_ok(),
        "[batch_fail] 有效文档应成功，got: {:?}",
        results[0].as_ref().err()
    );

    // 第二个（损坏 PDF）必须是 Err——不得伪装成 Ok(空串)
    assert!(
        results[1].is_err(),
        "[batch_fail] 损坏 PDF 必须走 Err 通道，got Ok(len={})",
        results[1].as_ref().unwrap().len()
    );
    // ADR-0006 §3：损坏 PDF 走 `code()=="malformed"` 分类。
    // force_ocr 路径下 text_layer_markdown 仍做加密预检 → 返 `Err(Malformed)`
    // （pdf-inspector 对 `%PDF`+垃圾返 InvalidStructure）→ batch.rs §5 直接标错。
    // 非 force_ocr 路径下走同一 text_layer 入口，错误码一致。
    let e = results[1].as_ref().err().unwrap();
    assert_eq!(
        e.code(),
        "malformed",
        "[batch_fail] 损坏 PDF 应 code()==malformed，实际 {}（{e}）",
        e.code()
    );

    // 清理临时文件
    let _ = std::fs::remove_file(&bogus);
    let _ = std::fs::remove_dir(&bogus_dir);
    println!("[batch_fail] OK: 有效文档成功 + 损坏 PDF 正确标 Err");
}
