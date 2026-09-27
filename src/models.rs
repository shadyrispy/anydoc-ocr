//! OCR 模型档：CLI 与库的默认档是 MinerU-4 basic 复刻（流程 + 模型 + 后处理），
//! 另三档小模型留给内存受限环境、离线安装包与精度校准。`--ocr-tier` 切换，无需重编译。
use clap::ValueEnum;

/// 可能的取值顺序即 `--ocr-tier` 的取值列表（`tiny|small|medium|mineru-basic`）；
/// 声明顺序与档位语义无关，**默认档由 `#[default]` 决定**。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, ValueEnum)]
pub enum OcrTier {
    /// 极速：PP-OCRv6 tiny（det 1.7MB / rec 4.3MB），常见中文文档够用
    Tiny,
    /// 均衡：PP-OCRv6 small（det 9.4MB / rec 20.2MB），中文覆盖最全
    Small,
    /// 高精度：PP-OCRv6 medium（det 59MB / rec 73MB），复杂版式（ARM CPU 慢）
    Medium,
    /// **CLI 与库的默认档**。MinerU-4 basic 档复刻：PP-DocLayoutV2 版面 +
    /// PP-OCRv6 tiny_det/small_rec + PP-FormulaNet_plus-M 公式，版面后处理对齐
    /// MinerU（score 0.45、paddlex filter、IoU 去重、header/footer 重标）。
    ///
    /// 主链路七件（版面/det/rec/词典/表格三件）都在 ModelScope 注册表内，
    /// 首跑走 `$OAR_HOME` auto-download（合计约 240MB，其中 `pp-doclayoutv2.onnx`
    /// 占 214MB，故 CLI 会先打一行下载提示再开跑，不让人对着静默等待猜）。
    /// **唯一可选项是公式识别两件**：`formula_m.onnx`（591MB）+
    /// `ppformulanet_tokenizer.json` 不在注册表、永不自动下载，只能由
    /// `ANYDOC_MODEL_DIR` 提供；缺省时 [`build_analyzer`] 跳过
    /// `with_formula_recognition`，正文/表格照常出，只是公式块不出 LaTeX
    /// （见 [`mineru_formula_ready`]）。
    #[default]
    MineruBasic,
}

/// `--help` 末尾的档位说明（`#[command(after_help)]`）。
///
/// 默认档变了以后，最需要回答的问题是"我需不需要传参数"——答案是不需要，
/// 除非默认档在这台机器上跑不起来。这段直接写进 `--help`，免得再去翻 README。
pub const MINERU_ENGINE_HELP: &str = "\
输入格式（#12）：
  PDF / OFD / HTML / CSV/TSV / office 系之外，位图也可直投：
  png jpg jpeg webp gif bmp tiff jp2（与 MinerU IMAGE_EXTENSIONS 一致）。
  · gif / tiff 只取首帧（多帧动画、多页 TIFF 的其余帧不进输出）。
  · jp2 能识别但没有 JPEG2000 解码器 → 显式报 unsupported，不会静默走别的通道。
  · 长边 > ANYDOC_RENDER_EDGE_CAP（默认 3500px）在解码前显式报 resourceLimit，
    本仓不对用户原图做静默缩放；要处理大图请先自行等比缩放。
  · EXIF 方向自动转正后再 OCR。

对齐口径（重要，别拿错基线）：
  本仓对齐的是 MinerU 的 basic 档（=hybrid effort medium，`tier.py:14`），
  不含 VLM。MinerU 自己的默认档是 standard（`tier.py:53` `tier=None → \"standard\"`，
  effort=high），standard 且未配 `server_url` 时要额外装本地 VLM 引擎（`tier.py:75`）。
  也就是说 `mineru` 默认跑出来的结果不是本仓的对比对象——多栏阅读顺序、图表内容
  分析这些归 VLM 的活本仓刻意不做，精度差异属档位差异，不是 bug（取舍理由见
  BACKLOG.md #14：与「CPU / 离线 / 单文件分发」的仓定位正面冲突）。真要 standard
  精度，唯一务实路径是加 `--server-url` 当客户端，不搬权重。
  另：MinerU 的 LLM 辅助后处理（`title_leveling` / `cross_page_table_cell_merge`）
  默认全关（`config.py:395-397`），本仓对应实现走规则路径（标题三信号投票 /
  跨页表几何列数对齐），语义上比它的默认更确定。

引擎档（--ocr-tier）说明：
  默认 mineru-basic：与 MinerU 4.0 basic 档同流程、同模型、同版面后处理。
  日常使用无需传任何参数；首跑会联网拉模型（合计约 240MB，$OAR_HOME 缓存复用）。

  公式识别（可选）：设 ANYDOC_MODEL_DIR 指向 mineru-ocr 资产目录，且目录内含
    formula_m.onnx + ppformulanet_tokenizer.json —— 缺这两件只是不出公式 LaTeX，
    正文与表格不受影响（两件不在 ModelScope 注册表，不会自动下载）。

  仅以下场景需要显式降档 --ocr-tier tiny|small|medium：
    · 内存受限：mineru-basic 峰值 ≈1.9GB，tiny 档 ≈0.5GB（实测 4 核 x86_64）
    · 完全离线的目标机：用小模型档（包内可带）
    · 精度校准 / A-B 对比
";

/// MinerU 默认档**必需**资产（全部在 ModelScope 注册表内，可 auto-download）。
///
/// 顺序固定，供 [`mineru_asset_status`] 预检与 CLI 首跑提示共用同一份真相；
/// 与 `spec_for(MineruBasic)` 的字段一一对应（单测 `mineru_assets_match_spec` 钉死）。
pub const MINERU_ASSETS: &[&str] = &[
    "pp-doclayoutv2.onnx",
    "pp-ocrv6_tiny_det.onnx",
    "pp-ocrv6_small_rec.onnx",
    "ppocrv6_dict.txt",
    "slanet_plus.onnx",
    "pp-lcnet_x1_0_table_cls.onnx",
    "table_structure_dict_ch.txt",
];

/// 公式识别两件（不在 ModelScope 注册表 → 只能由 `ANYDOC_MODEL_DIR` 提供）。
///
/// 缺这两件**不阻断 MinerU 主流程**（版面/det/rec/表格全在，只是公式块不出
/// LaTeX），所以 [`crate::ocr_engine::build_analyzer`] 据此条件挂载
/// `with_formula_recognition`，而不是让整个默认档不可用。
pub const MINERU_FORMULA_ASSETS: &[&str] = &["formula_m.onnx", "ppformulanet_tokenizer.json"];

/// 公式识别资产是否就位（`ANYDOC_MODEL_DIR` 下两件齐全）。
///
/// 判据与运行期加载路径严格一致：`ocr_engine::model_path` 先看 `ANYDOC_MODEL_DIR`
/// 下的绝对路径，缺则回裸名交给上游 `download::resolve_path`——后者第一条规则是
/// "该路径已存在即原样信任"（裸名即相对当前工作目录）。两条都算就位，否则会
/// 把"模型就放在运行目录"这种用法误判成缺件。
pub fn mineru_formula_ready(model_dir: Option<&str>) -> bool {
    let exists = |name: &str| {
        model_dir
            .filter(|d| !d.is_empty())
            .is_some_and(|d| std::path::Path::new(d).join(name).is_file())
            || std::path::Path::new(name).is_file()
    };
    MINERU_FORMULA_ASSETS.iter().all(|name| exists(name))
}

/// MinerU 默认档必需资产的就位情况（CLI 起跑前的预检输入）。
#[derive(Debug, Default, PartialEq, Eq)]
pub struct MineruAssetStatus {
    /// 注册表认识但 `$OAR_HOME` 缓存里还没校验就位 → 首跑要联网下载。
    /// 不阻断（下载能成），但 CLI 先把清单和体积说出来，避免"静默卡住"的观感。
    pub needs_download: Vec<&'static str>,
    /// [`Self::needs_download`] 各条目的注册表体积合计（字节）。
    pub download_bytes: u64,
}

impl MineruAssetStatus {
    /// 是否需要联网（true = 首跑要下载模型）。
    pub fn needs_download(&self) -> bool {
        !self.needs_download.is_empty()
    }
}

/// 判定默认档（MinerU）必需资产在当前环境下的就位情况。
///
/// 解析规则与 `ocr_engine::model_path` + 上游 `download::resolve_path` 逐条一致，
/// 故结论可信：
/// 1. `ANYDOC_MODEL_DIR` 非空且目录下该文件存在 → 就位（绝对路径直载，不校验 hash）；
/// 2. 否则看 `$OAR_HOME`（默认 `~/.oar`，同 `download::cache_dir`）里是否为
///    **已校验**缓存——判据是模型文件在且 `.sha256` 伴生在（伴生文件只在 hash
///    校验通过后写入）；不满足则记 [`MineruAssetStatus::needs_download`]。
///
/// 只判存在性与校验标记，不读模型字节（预检要在毫秒级返回）。
pub fn mineru_asset_status(model_dir: Option<&str>, oar_home: Option<&str>) -> MineruAssetStatus {
    use oar_ocr::download::{cache_dir, find};
    let home = match oar_home.filter(|h| !h.is_empty()) {
        Some(h) => std::path::PathBuf::from(h),
        None => cache_dir(),
    };
    let dir = model_dir.filter(|d| !d.is_empty());
    let mut st = MineruAssetStatus::default();
    for name in MINERU_ASSETS {
        if dir.is_some_and(|d| std::path::Path::new(d).join(name).is_file()) {
            continue;
        }
        // 未注册的名字不会被 auto-download 满足；MINERU_ASSETS 与注册表的一致性
        // 由单测保证，这里缺失只记为"下载不了"而不单列，避免多一个从未走到的分支。
        let Some(entry) = find(name) else {
            st.needs_download.push(name);
            continue;
        };
        let cached =
            home.join(entry.name).is_file() && home.join(format!(".{}.sha256", entry.name)).is_file();
        if !cached {
            st.needs_download.push(name);
            st.download_bytes += entry.size;
        }
    }
    st
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
    /// 印章文字检测（DB 变体，输出摆正后的多边形行框）；印章识别**默认开**（#10b，
    /// `ANYDOC_NO_SEAL_OCR` 关闭），经 `ocr_post` 懒建 session 加载——页面无 Seal
    /// 版面元素时不触达（`seal_pass` 早退），故不含章的文档不会加载它。
    /// 注册表名走 auto-download；
    /// `ANYDOC_MODEL_DIR` 下另有 MinerU 命名的同名资产时优先本地文件
    /// （解析顺序见 `ocr_post::seal_model_candidates`）。
    pub seal_det: &'static str,
    /// 无线表格**单元格检测**（#7 A/B 用，`ANYDOC_WIRELESS_CELLS` 存在才挂）。
    /// 空串 = 该档没有候选件，开关给了也不接（tiny/small/medium 不留这条口子，
    /// 免得 129MB 模型被误算进小档）。
    pub wireless_cell_det: &'static str,
}

/// #7 的 A/B 开关：`ANYDOC_WIRELESS_CELLS` 存在即给 mineru-basic 档接
/// `rt-detr-l_wireless_table_cell_det.onnx` + cells→HTML 通路。
///
/// 为什么先做成环境变量而不是直接改默认：**第 0 步实测显示现状（slanet_plus
/// 当通用兜底）在无线表上已经能出正确的行列与 colspan/rowspan**，而这条通路要
/// 多加载一个 129MB 模型（默认档从 7 件/≈240MB 涨到 8 件/≈370MB）、每页再付一次
/// 单元格检测推理——在"增益未证"之前把它塞进默认路径就是拿确定的成本赌不确定的
/// 收益。开关留在这里，是为了让现网/我们的语料能一行 env 出对照结论。
///
/// 资产来路：`rt-detr-l_wireless_table_cell_det.onnx` 在注册表内
/// （`oar-ocr-core/src/core/download/registry.rs:104`，129,331,821 B），可
/// auto-download；故**不计入** `MINERU_ASSETS`（那是"默认档首跑必需"的承诺，
/// 开关关时不该承诺，也不该让预检多算 129MB）。缺件时 `build_analyzer` 不静默
/// 回落，而是打一行说明——否则"开了开关没生效"最难查。
pub fn wireless_cells_wanted() -> bool {
    static V: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *V.get_or_init(|| std::env::var("ANYDOC_WIRELESS_CELLS").is_ok())
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
            seal_det: "pp-ocrv4_mobile_seal_det.onnx",
            wireless_cell_det: "",
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
            seal_det: "pp-ocrv4_mobile_seal_det.onnx",
            wireless_cell_det: "",
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
            seal_det: "pp-ocrv4_mobile_seal_det.onnx",
            wireless_cell_det: "",
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
            // MinerU 配套资产（seal_PP-OCRv4_det）文件名不同、hash 不在注册表，
            // 走 ANYDOC_MODEL_DIR 本地优先（candidates 顺序见 ocr_post）。
            seal_det: "pp-ocrv4_mobile_seal_det.onnx",
            // #7：mineru-basic 是唯一有该候选件的档（129MB 不进小档），且只有
            // `ANYDOC_WIRELESS_CELLS` 给出时才加载。
            wireless_cell_det: "rt-detr-l_wireless_table_cell_det.onnx",
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

    /// 默认档 = MinerU 复刻档。CLI 的 `default_value_t` 与库的 `Default` 都从这里
    /// 取，故这一条断言同时钉住"命令行默认走 MinerU 流程和模型"这个需求本身。
    #[test]
    fn default_tier_is_mineru() {
        assert_eq!(OcrTier::default(), OcrTier::MineruBasic);
        assert!(OcrTier::default().is_mineru());
    }

    /// `MINERU_ASSETS` 必须与 `spec_for(MineruBasic)` 的**必需**件逐一对应，
    /// 且不含公式两件——否则预检清单和实际加载的模型会静默脱节（比如 spec 换了
    /// rec 模型但清单没跟上，预检通过、运行期却下载不到）。
    #[test]
    fn mineru_assets_match_spec() {
        let s = spec_for(OcrTier::MineruBasic);
        let expect = [
            s.layout,
            s.det,
            s.rec,
            s.dict,
            s.table_structure,
            s.table_cls,
            s.table_dict,
        ];
        assert_eq!(MINERU_ASSETS, expect, "MINERU_ASSETS 与 spec 脱节");
        for name in MINERU_ASSETS {
            assert!(
                !MINERU_FORMULA_ASSETS.contains(name),
                "{name} 不能同时是必需件和可选件（预检会把它算进必需清单）"
            );
        }
        assert_eq!(MINERU_FORMULA_ASSETS, [s.formula, s.formula_tokenizer]);
        assert!(s.doc_ori.is_empty(), "MinerU basic 无方向矫正，spec 变更需同步预检");
    }

    /// 必需件全部注册在案 → 首跑能靠 auto-download 补齐；公式两件恰恰不注册，
    /// 这才是"公式可选、其余必需"这条分界的依据。
    #[test]
    fn required_assets_are_downloadable_and_formula_ones_are_not() {
        use oar_ocr::download::find;
        for name in MINERU_ASSETS {
            assert!(find(name).is_some(), "{name} 不在注册表 → 预检无法承诺首跑可补齐");
        }
        for name in MINERU_FORMULA_ASSETS {
            assert!(find(name).is_none(), "{name} 竟然可下载 → 该挪进 MINERU_ASSETS 了");
        }
    }

    /// 预检：空目录 + 空缓存 → 7 件全部待下载，体积合计与注册表一致。
    #[test]
    fn preflight_reports_all_missing_when_env_empty() {
        let st = mineru_asset_status(Some("/nonexistent-anydoc-dir"), Some("/nonexistent-oar"));
        assert_eq!(st.needs_download, MINERU_ASSETS.to_vec());
        assert!(st.needs_download());
        let want: u64 = MINERU_ASSETS
            .iter()
            .filter_map(|n| oar_ocr::download::find(n))
            .map(|e| e.size)
            .sum();
        assert_eq!(st.download_bytes, want, "体积合计应取注册表条目之和");
    }

    /// 预检：`ANYDOC_MODEL_DIR` 里放齐 7 件 → 零下载（直载绝对路径，不校验 hash，
    /// 与 `model_path` 语义一致）。
    #[test]
    fn preflight_passes_when_model_dir_has_all_files() {
        let dir = std::env::temp_dir().join(format!("anydoc_preset_{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("mkdir");
        for name in MINERU_ASSETS {
            std::fs::write(dir.join(name), b"x").expect("write stub");
        }
        let st = mineru_asset_status(
            Some(dir.to_str().unwrap()),
            Some("/nonexistent-oar"),
        );
        assert!(!st.needs_download(), "目录内齐全仍报缺件: {:?}", st.needs_download);
        assert_eq!(st.download_bytes, 0);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// 预检：部分缺件只报缺的那几件（守护"点名"能力——报错文案要能指到具体模型）。
    #[test]
    fn preflight_names_only_the_missing_ones() {
        let dir = std::env::temp_dir().join(format!("anydoc_partial_{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("mkdir");
        for name in MINERU_ASSETS {
            std::fs::write(dir.join(name), b"x").expect("write stub");
        }
        std::fs::remove_file(dir.join("pp-doclayoutv2.onnx")).expect("remove one");
        let st = mineru_asset_status(Some(dir.to_str().unwrap()), Some("/nonexistent-oar"));
        assert_eq!(st.needs_download, vec!["pp-doclayoutv2.onnx"]);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// 空字符串 env 视同未设置（`model_path` 也是这么处理的），否则 `VAR=` 会被
    /// 当成一个真实目录名而全量误判缺件。
    #[test]
    fn empty_env_values_are_treated_as_unset() {
        let a = mineru_asset_status(Some(""), Some(""));
        let b = mineru_asset_status(None, None);
        assert_eq!(a, b, "空 env 应与未设置同结论");
    }

    /// 公式就位判据：目录里必须**两件齐全**才算就位（缺一件时上游会加载失败，
    /// 半挂状态比不挂更糟）。
    #[test]
    fn formula_ready_needs_both_files() {
        let dir = std::env::temp_dir().join(format!("anydoc_formula_{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("mkdir");
        let d = dir.to_str().unwrap();
        assert!(!mineru_formula_ready(Some(d)), "空目录不应判就位");
        assert!(!mineru_formula_ready(None), "未设 ANYDOC_MODEL_DIR 不应判就位");
        std::fs::write(dir.join(MINERU_FORMULA_ASSETS[0]), b"x").expect("write");
        assert!(!mineru_formula_ready(Some(d)), "只有一件不应判就位");
        std::fs::write(dir.join(MINERU_FORMULA_ASSETS[1]), b"x").expect("write");
        assert!(mineru_formula_ready(Some(d)), "两件齐全应判就位");
        std::fs::remove_dir_all(&dir).ok();
    }
}

