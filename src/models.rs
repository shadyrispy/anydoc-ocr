//! OCR 模型档：极速/均衡/高精度三档，CLI 参数切换，无需重编译。
use clap::ValueEnum;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, ValueEnum)]
pub enum OcrTier {
    /// 极速：PP-OCRv6 tiny（det 1.7MB / rec 4.3MB），常见中文文档够用
    #[default]
    Tiny,
    /// 均衡：PP-OCRv6 small（det 9.4MB / rec 20.2MB），中文覆盖最全
    Small,
    /// 高精度：PP-OCRv6 medium（det 59MB / rec 73MB），复杂版式（ARM CPU 慢）
    Medium,
    /// MinerU-4 basic 档复刻：PP-DocLayoutV2 + PP-OCRv6 tiny_det/small_rec，
    /// 版面后处理对齐 MinerU（score 0.45、paddlex filter、IoU 去重、header/footer 重标）。
    /// 模型资产需自备（`ANYDOC_MODEL_DIR` 指向 mineru-ocr 目录）。
    MineruBasic,
}

impl OcrTier {
    /// 下一档（Tiny→Small→Medium）。最高档返回 `None`——按页重试只升一档、
    /// 不递归，防无限循环（T2）。
    pub fn next(self) -> Option<OcrTier> {
        match self {
            OcrTier::Tiny => Some(OcrTier::Small),
            OcrTier::Small => Some(OcrTier::Medium),
            OcrTier::Medium => None,
            OcrTier::MineruBasic => None,
        }
    }

    /// MinerU 复刻档：版面后处理与阈值走 MinerU 语义（core 内 mineru_post_process）。
    pub fn is_mineru(self) -> bool {
        matches!(self, OcrTier::MineruBasic)
    }
}

/// 版面模型选择：默认文档结构 vs 表格专用（检出 Table 才跑 SLANet）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, ValueEnum)]
pub enum OcrLayout {
    /// 默认文档版面（PP-DocLayout-S，兼顾文字/标题/表格）
    #[default]
    Doc,
    /// 表格专用版面（PicoDet-Layout-1x-Table，只标 Table；GFM 文本流仍按坐标输出）
    Table,
}

/// 模型规格：auto-download 键名（ModelScope greatv/oar-ocr）
#[derive(Debug, Clone)]
pub struct ModelSpec {
    pub layout: &'static str,
    pub layout_name: &'static str,
    pub det: &'static str,
    pub rec: &'static str,
    pub dict: &'static str,
    pub table_structure: &'static str,
    pub table_cls: &'static str,
    pub table_dict: &'static str,
    pub doc_ori: &'static str,
    /// 公式识别（PP-FormulaNet_plus-M ONNX + 内嵌 fast_tokenizer JSON）；
    /// 空串 = 该档不接公式（build_analyzer 据此跳过 with_formula_recognition）。
    pub formula: &'static str,
    pub formula_tokenizer: &'static str,
}

pub fn spec_for(tier: OcrTier) -> ModelSpec {
    match tier {
        OcrTier::Tiny => ModelSpec {
            layout: "pp-doclayout-s.onnx",
            layout_name: "PP-DocLayout-S",
            det: "pp-ocrv6_tiny_det.onnx",
            rec: "pp-ocrv6_tiny_rec.onnx",
            dict: "ppocrv6_tiny_dict.txt",
            table_structure: "slanet_plus.onnx",
            table_cls: "pp-lcnet_x1_0_table_cls.onnx",
            table_dict: "table_structure_dict_ch.txt",
            doc_ori: "pp-lcnet_x1_0_doc_ori.onnx",
            formula: "",
            formula_tokenizer: "",
        },
        OcrTier::Small => ModelSpec {
            layout: "pp-doclayout-m.onnx",
            layout_name: "PP-DocLayout-M",
            det: "pp-ocrv6_small_det.onnx",
            rec: "pp-ocrv6_small_rec.onnx",
            dict: "ppocrv6_dict.txt",
            table_structure: "slanet_plus.onnx",
            table_cls: "pp-lcnet_x1_0_table_cls.onnx",
            table_dict: "table_structure_dict_ch.txt",
            doc_ori: "pp-lcnet_x1_0_doc_ori.onnx",
            formula: "",
            formula_tokenizer: "",
        },
        OcrTier::Medium => ModelSpec {
            layout: "pp-doclayoutv3.onnx",
            layout_name: "PP-DocLayoutV3",
            det: "pp-ocrv6_medium_det.onnx",
            rec: "pp-ocrv6_medium_rec.onnx",
            dict: "ppocrv6_dict.txt",
            table_structure: "slanet_plus_v2.onnx",
            table_cls: "pp-lcnet_x1_0_table_cls.onnx",
            table_dict: "table_structure_dict_ch.txt",
            doc_ori: "pp-lcnet_x1_0_doc_ori.onnx",
            formula: "",
            formula_tokenizer: "",
        },
        // MinerU-4 basic 复刻（§5.2 Step 1）：与 MinerU `basic` 档逐模型对齐。
        // det/rec/dict 与 oar tiny/small 缓存字节一致；版面换 DocLayoutV2 并走
        // MinerU 后处理链。资产目录：ANYDOC_MODEL_DIR=/data/models/mineru-ocr。
        // table 三件 Step 2 接线；doc_ori 置空（MinerU basic 无页面方向矫正，
        // build_analyzer 据此跳过 with_document_orientation）。
        OcrTier::MineruBasic => ModelSpec {
            layout: "pp-doclayoutv2.onnx",
            layout_name: "PP-DocLayoutV2",
            det: "pp-ocrv6_tiny_det.onnx",
            rec: "pp-ocrv6_small_rec.onnx",
            dict: "ppocrv6_dict.txt",
            table_structure: "slanet_plus.onnx",
            table_cls: "pp-lcnet_x1_0_table_cls.onnx",
            table_dict: "table_structure_dict_ch.txt",
            doc_ori: "",
            formula: "formula_m.onnx",
            formula_tokenizer: "ppformulanet_tokenizer.json",
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tier_next_chain() {
        assert_eq!(OcrTier::Tiny.next(), Some(OcrTier::Small));
        assert_eq!(OcrTier::Small.next(), Some(OcrTier::Medium));
        assert_eq!(OcrTier::Medium.next(), None, "最高档不再升级，防循环");
    }
}

