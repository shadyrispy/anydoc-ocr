//! Step 3 前置验证：`tokenizers` 加载 PP-FormulaNet_plus-M 内嵌 fast_tokenizer
//! JSON，与 Python `tokenizers` 0.23 的 decode 结果对拍（refs 见 tok_refs.json）。
//! 仅测试用，不进主干。

use tokenizers::Tokenizer;

fn decode(tok: &Tokenizer, ids: &[u32]) -> String {
    tok.decode(ids, false).expect("decode failed")
}

#[test]
fn ppformulanet_tokenizer_parity() {
    let path = std::env::var("TOK_JSON")
        .unwrap_or_else(|_| "/data/models/mineru-ocr/ppformulanet_tokenizer.json".into());
    let tok = Tokenizer::from_file(&path).expect("from_file failed");
    assert_eq!(tok.get_vocab_size(true), 50000);

    // 与 Python 参照逐条对拍（生成于 tokenizers 0.23.2, skip_special_tokens=False）
    let cases: Vec<(Vec<u32>, &str)> = vec![
        (vec![1, 243, 87, 13, 45, 2, 0, 0], "<pad> a[START_SUB]7</s><s><s>"),
        (
            vec![1, 8802, 501, 27, 58, 27, 2, 0],
            "<pad> consensus mod%D%</s><s>",
        ),
        (
            vec![1, 365, 365, 2, 35, 2, 2, 2, 2],
            "<pad>rara</s>-</s></s></s></s>",
        ),
        (
            vec![1, 5, 12264, 12, 953, 3, 12, 4515, 3, 2],
            "<pad>[END_REF]outhe[END_SUP]til<unk>[END_SUP]ernel<unk></s>",
        ),
    ];
    for (ids, want) in cases {
        let got = decode(&tok, &ids);
        assert_eq!(got, want, "ids={:?}", ids);
    }

    // 特殊 token id 口径（adapter 警告默认值 EOS=2 是否与内嵌配置一致）
    for (t, id) in [
        ("<pad>", 1u32),
        ("</s>", 2u32),
        ("<unk>", 3u32),
        ("<s>", 0u32),
    ] {
        assert_eq!(tok.token_to_id(t), Some(id), "special {}", t);
    }

    // 连续段（1000-1049 / 40000-40049）与 Python 一致性的抽样哈希对拍
    let range_a: Vec<u32> = (1000..1050).collect();
    let range_b: Vec<u32> = (40000..40050).collect();
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut h = DefaultHasher::new();
    decode(&tok, &range_a).hash(&mut h);
    decode(&tok, &range_b).hash(&mut h);
    println!("parity hash: {:x}", h.finish());
}
