//! OCR 后处理层（#10b 起印章识别**默认开启**，回捞仍默认关闭）：
//!
//! - 印章文字识别（backlog #5 / #10b，默认开，`ANYDOC_NO_SEAL_OCR` 关闭）：
//!   版面模型已输出 `Seal` 元素，但实测（见 [`crate::seal`] 模块文档）上游
//!   `with_seal_text_detection` 只检测不识别、且会禁用跨页批量 OCR。本层
//!   不动 vendored 管线：Seal bbox → 页图裁剪 → 印章 DB 检测摆正分行 →
//!   tier 同款 rec 识别 → 写回 `LayoutElement.text`，由 `gfm_adapter` 输出
//!   `【印章】…` 行。
//!   默认开是对齐 MinerU：basic（medium）档就跑印章 OCR
//!   （`backend/analysis/pdf/window.py:529-531`），且 `seal_det` 是 basic 必需件。
//! - `ANYDOC_TABLE_FILL`（存在即开启）——空单元格 OCR 回捞（backlog #4）：
//!   对齐 MinerU flash 表填充（utils/tables.py）：bbox 裁剪 → 按单元格角度
//!   摆正 → DB 检测（box_thresh 0.5 / unclip 1.6，不做合并）→ 行文本写回
//!   `TableCell.text` → 用 `structure_tokens` 重新生成 `html_structure`
//!   （td 序与 (row,col) 网格对齐，镜像上游 `collect_cell_texts_for_tokens`）。
//!
//! 失败语义（两路一致）：**绝不阻断主链路**——模型缺失/加载失败/单框推理
//! 失败都只 `eprintln` 告警一次并跳过对应填充；主 OCR 结果原样返回。
//! 模型 session 在首次用到时才建（懒加载），故印章识别虽然默认开，**页面上没有
//! Seal 元素就完全不碰印章模型**（`seal_pass` 早退，见其实现）——不含章的文档
//! 零成本，只有含章页多一次 4.8MB 模型加载。
use std::sync::{Arc, Mutex, OnceLock};

use image::RgbImage;
use oar_ocr::domain::structure::{LayoutElementType, StructureResult, TableResult};
use oar_ocr::predictors::{
    SealTextDetectionPredictor, TextDetectionPredictor, TextRecognitionPredictor,
};
use oar_ocr::processors::{BoundingBox, CellGridInfo, parse_cell_grid_info, wrap_table_html_with_content};
use oar_ocr::utils::{BBoxCrop, get_rotate_crop_image};

use crate::models::{OcrTier, spec_for};
use crate::seal::{SEAL_IOU_DEDUPE, dedupe_overlapping, join_seal_lines};

/// 印章识别开关（**#10b 起默认开**，`ANYDOC_NO_SEAL_OCR` 存在即关闭）。
///
/// 为什么默认开：MinerU 在 basic（= effort medium）档就跑印章 OCR
/// （`window.py:529-531`）且把 `seal_det` 列为 basic 必需件；我们对齐的是同一档，
/// 默认不出 `【印章】…` 行就是**低于** MinerU basic 的输出面。
///
/// 为什么代价可控：`seal_pass` 在页面无 `Seal` 版面元素时直接早退，一个模型都不
/// 加载——成本只落在真含章的页上（多 4.8MB 检测模型 + 每章一次 det+rec）。
/// 模型缺失/加载失败只告警一次并跳过，主链路结果原样返回（离线包因此不会红）。
///
/// 关闭名用 `NO_` 前缀而非复用 `ANYDOC_SEAL_OCR`：同一个变量名在历史上是
/// "存在即开启"，若把它翻转成"存在即关闭"，老脚本里的 `ANYDOC_SEAL_OCR=1`
/// 就静默变成"关闭印章"——那是最坏的漂移。故老变量**保留为等价默认值的别名**
/// （设不设都是开），唯一的关闭入口是 `ANYDOC_NO_SEAL_OCR`（存在即关闭、不限值，
/// 与 `ANYDOC_NO_HYBRID` 同一族语义）。两个同时给出时以关闭为准并告警一次。
///
/// 进程内定格（同 `session_pool_wanted` 模式）：`EngineKey` 含此值，中途改环境
/// 变量不会出现"引擎有/无后处理"错配。
pub fn seal_on() -> bool {
    static V: OnceLock<bool> = OnceLock::new();
    *V.get_or_init(|| seal_on_from(
        std::env::var("ANYDOC_NO_SEAL_OCR").ok().as_deref(),
        std::env::var("ANYDOC_SEAL_OCR").ok().as_deref(),
    ))
}

/// 开关内核（纯函数，可单测）：`off` = `ANYDOC_NO_SEAL_OCR`，`legacy` = 老变量。
fn seal_on_from(off: Option<&str>, legacy: Option<&str>) -> bool {
    if off.is_some() {
        if legacy.is_some() {
            eprintln!(
                "[anydoc-ocr] 提示：ANYDOC_SEAL_OCR 与 ANYDOC_NO_SEAL_OCR 同时设置，以关闭为准\
                （印章文字将不进输出）"
            );
        }
        return false;
    }
    true
}

/// 空单元格回捞开关（同上）。
pub fn table_fill_on() -> bool {
    static V: OnceLock<bool> = OnceLock::new();
    *V.get_or_init(|| std::env::var("ANYDOC_TABLE_FILL").is_ok())
}

/// 印章检测模型解析候选（纯函数，便于单测）：
/// 1. `ANYDOC_MODEL_DIR` 下 MinerU 文件名（`seal_PP-OCRv4_det_infer.onnx` →
///    本仓资产目录实际落名 `seal_ppocrv4_det.onnx`，两名为兼容都探）；
/// 2. `ANYDOC_MODEL_DIR` 下注册表名（自备 hash 匹配版）；
/// 3. 裸注册表名（auto-download，需网络）。
/// `model_dir` 空 = 未设置。返回 (路径或裸名, 是否绝对路径信任)。
pub fn seal_model_candidates(model_dir: &str, registry_name: &str) -> Vec<(String, bool)> {
    let mut out = Vec::new();
    if !model_dir.is_empty() {
        let dir = std::path::Path::new(model_dir);
        for name in ["seal_ppocrv4_det.onnx", "seal_PP-OCRv4_det_infer.onnx", registry_name] {
            let p = dir.join(name);
            if p.is_file() {
                out.push((p.to_string_lossy().into_owned(), true));
            }
        }
    }
    out.push((registry_name.to_string(), false));
    out
}

/// 单个框的推理降级守卫：模型构建失败一次后不再重试（warn 一次）。
enum Lazy<T> {
    Pending,
    Built(T),
    Dead,
}

/// 后处理模型集（按 tier 一份；引擎缓存键已含开关位）。
pub struct PostPass {
    tier: OcrTier,
    /// 印章 DB 检测（仅 seal_on 时用）。Arc 化：取句柄时克隆 Arc、不跨推理持锁。
    seal_det: Mutex<Lazy<Arc<SealTextDetectionPredictor>>>,
    /// 通用文本 DB 检测（仅 table_fill_on 时用；box_thresh/unclip 对齐 MinerU）
    cell_det: Mutex<Lazy<Arc<TextDetectionPredictor>>>,
    /// 行识别（两路共用，随 tier）
    rec: Mutex<Lazy<Arc<TextRecognitionPredictor>>>,
    /// 后处理推理互斥：standalone predictor 每模型单 session，与
    /// `OcrEngine::infer_lock` 的 P0-1b 论证同理。
    infer_lock: Mutex<()>,
}

fn lazy_build<T>(slot: &Mutex<Lazy<Arc<T>>>, what: &str, f: impl FnOnce() -> Result<T, String>) {
    let mut g = slot.lock().unwrap_or_else(|p| p.into_inner());
    if matches!(*g, Lazy::Pending) {
        *g = match f() {
            Ok(v) => Lazy::Built(Arc::new(v)),
            Err(e) => {
                eprintln!("[anydoc-ocr] 警告：{what} 初始化失败（{e}），本次转换跳过该后处理（主链路不受影响）");
                Lazy::Dead
            }
        };
    }
}

/// 克隆已就绪 predictor 的 Arc 句柄（Pending/Dead → None）。锁只保护槽位
/// 状态机，推理在锁外进行（Arc clone 即出锁，无跨推理持锁）。
fn built<T>(slot: &Mutex<Lazy<Arc<T>>>) -> Option<Arc<T>> {
    let g = slot.lock().ok()?;
    match &*g {
        Lazy::Built(v) => Some(Arc::clone(v)),
        _ => None,
    }
}

impl PostPass {
    pub fn new(tier: OcrTier) -> Self {
        Self {
            tier,
            seal_det: Mutex::new(Lazy::Pending),
            cell_det: Mutex::new(Lazy::Pending),
            rec: Mutex::new(Lazy::Pending),
            infer_lock: Mutex::new(()),
        }
    }

    pub fn wanted() -> bool {
        seal_on() || table_fill_on()
    }

    fn ensure_rec(&self) {
        let spec = spec_for(self.tier);
        let (rec, dict) = (crate::ocr_engine::model_path_for_post(spec.rec), crate::ocr_engine::model_path_for_post(spec.dict));
        lazy_build(&self.rec, "行识别模型（后处理）", || {
            TextRecognitionPredictor::builder()
                .dict_path(dict)
                .build(rec)
                .map_err(|e| e.to_string())
        });
    }

    fn ensure_seal_det(&self) {
        let spec = spec_for(self.tier);
        let dir = std::env::var("ANYDOC_MODEL_DIR").unwrap_or_default();
        // 候选逐个试建（绝对路径不存在/加载失败 → 下一个；最后裸名走 auto-download）
        let candidates = seal_model_candidates(&dir, spec.seal_det);
        lazy_build(&self.seal_det, "印章检测模型", || {
            let mut last = String::from("无候选");
            for (path, trusted) in candidates {
                if trusted && !std::path::Path::new(&path).is_file() {
                    continue;
                }
                match SealTextDetectionPredictor::builder().build(path.clone()) {
                    Ok(p) => return Ok(p),
                    Err(e) => last = format!("{path}: {e}"),
                }
            }
            Err(last)
        });
    }

    fn ensure_cell_det(&self) {
        let spec = spec_for(self.tier);
        let det = crate::ocr_engine::model_path_for_post(spec.det);
        lazy_build(&self.cell_det, "单元格文本检测模型", || {
            // MinerU flash 填充参数：box_thresh 0.5（低于默认 0.6，救低置信行）、
            // unclip 1.6（宽松外扩保边）。limit_* 与主 analyzer 的 mineru 档一致。
            let cfg = oar_ocr::domain::tasks::TextDetectionConfig {
                box_threshold: 0.5,
                unclip_ratio: 1.6,
                ..Default::default()
            };
            TextDetectionPredictor::builder()
                .with_config(cfg)
                .build(det)
                .map_err(|e| e.to_string())
        });
    }

    /// 对单页结果跑已开启的后处理（永不 Err；关闭的开关零触达）。
    pub fn run(&self, img: &RgbImage, res: &mut StructureResult) {
        if seal_on() {
            self.seal_pass(img, res);
        }
        if table_fill_on() {
            self.table_fill_pass(img, res);
        }
    }

    // ── #5 印章 ──

    fn seal_pass(&self, img: &RgbImage, res: &mut StructureResult) {
        let idxs: Vec<usize> = res
            .layout_elements
            .iter()
            .enumerate()
            .filter(|(_, e)| e.element_type == LayoutElementType::Seal)
            .map(|(i, _)| i)
            .collect();
        if idxs.is_empty() {
            return;
        }
        // IoU 去重（嵌套内外圈只留一个）
        let boxes: Vec<BoundingBox> = idxs.iter().map(|&i| res.layout_elements[i].bbox.clone()).collect();
        let kept = dedupe_overlapping(&boxes, SEAL_IOU_DEDUPE);
        if kept.is_empty() {
            return;
        }
        self.ensure_seal_det();
        self.ensure_rec();
        let _infer = self.infer_lock.lock().unwrap_or_else(|p| p.into_inner());
        for &k in &kept {
            let i = idxs[k];
            let Some(crop) = crop_clamped(img, &res.layout_elements[i].bbox) else { continue };
            let lines = self.recognize_seal(&crop);
            if let Some(text) = join_seal_lines(&lines) {
                res.layout_elements[i].text = Some(text);
            }
        }
    }

    /// 印章裁剪图 → 检测行框（摆正）→ 逐行 rec。任何一步失败 → 空 vec。
    fn recognize_seal(&self, crop: &RgbImage) -> Vec<String> {
        let Some(seal_det) = built(&self.seal_det) else {
            // 模型未就绪：ensure_seal_det 已 warn 过，这里静默
            return Vec::new();
        };
        let dets: Vec<_> = match seal_det
            .predict(vec![crop.clone()])
            .map(|r| r.detections.into_iter().next().unwrap_or_default())
        {
            Ok(d) => d,
            Err(e) => {
                warn_once("seal-det", &format!("印章检测失败（跳过该章）: {e}"));
                return Vec::new();
            }
        };
        // 行框 → 摆正 → rec。印章检测对**环排文字**输出沿弧走行的多顶点
        // 多边形，min-area-rect 摆正只会得到残缺字（实测「北京测试科技有限公司」
        // →「时技有限」），故按 [`crate::seal::is_curved_band`] 显式跳过，
        // 只识别近似直的行（章底「专用章」等）。弧行矫正见 backlog #5a。
        // y 升序（章内多行自上而下，同 MinerU SortPolyBoxes）。
        let mut rows: Vec<(f32, RgbImage)> = Vec::new();
        for d in &dets {
            if crate::seal::is_curved_band(&d.bbox.points) {
                continue;
            }
            let quad = d.bbox.get_min_area_rect().get_box_points();
            match get_rotate_crop_image(crop, &quad) {
                Ok(q) => rows.push((d.bbox.y_min(), q)),
                Err(_) => continue,
            }
        }
        rows.sort_by(|a, b| a.0.total_cmp(&b.0));
        let imgs: Vec<RgbImage> = rows.into_iter().map(|(_, im)| im).collect();
        if imgs.is_empty() {
            return Vec::new();
        }
        let Some(rec) = built(&self.rec) else { return Vec::new() };
        match rec.predict(imgs) {
            Ok(r) => r.texts,
            Err(e) => {
                warn_once("seal-rec", &format!("印章行识别失败（跳过该章）: {e}"));
                Vec::new()
            }
        }
    }

    // ── #4 空单元格回捞 ──

    fn table_fill_pass(&self, img: &RgbImage, res: &mut StructureResult) {
        let _infer = self.infer_lock.lock().unwrap_or_else(|p| p.into_inner());
        for table in &mut res.tables {
            if table.cells.is_empty() {
                continue;
            }
            let empties: Vec<usize> = table
                .cells
                .iter()
                .enumerate()
                .filter(|(_, c)| {
                    c.text.as_ref().map(|t| t.trim().is_empty()).unwrap_or(true)
                })
                .map(|(i, _)| i)
                .collect();
            if empties.is_empty() {
                continue;
            }
            self.ensure_cell_det();
            self.ensure_rec();
            let mut filled_any = false;
            for &ci in &empties {
                let Some(crop) = crop_clamped(img, &table.cells[ci].bbox) else { continue };
                // 单元格内 DB 检测 → 行框（含 0° 以外的小角度，rotate-crop 摆正）
                let Some(cell_det) = built(&self.cell_det) else { continue };
                let dets = match cell_det
                    .predict(vec![crop.clone()])
                    .map(|r| r.detections.into_iter().next().unwrap_or_default())
                {
                    Ok(d) => d,
                    Err(e) => {
                        warn_once("cell-det", &format!("单元格检测失败（跳过该格）: {e}"));
                        continue;
                    }
                };
                let mut rows: Vec<(f32, f32, RgbImage)> = Vec::new();
                for d in &dets {
                    if d.bbox.points.len() != 4 {
                        continue;
                    }
                    if let Ok(line) = get_rotate_crop_image(&crop, &d.bbox.points) {
                        rows.push((d.bbox.y_min(), d.bbox.x_min(), line));
                    }
                }
                if rows.is_empty() {
                    continue;
                }
                rows.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.total_cmp(&b.1)));
                let imgs: Vec<RgbImage> = rows.into_iter().map(|(_, _, im)| im).collect();
                let Some(rec) = built(&self.rec) else { continue };
                let texts = match rec.predict(imgs) {
                    Ok(r) => r.texts,
                    Err(e) => {
                        warn_once("cell-rec", &format!("单元格行识别失败（跳过该格）: {e}"));
                        continue;
                    }
                };
                let joined = join_seal_lines(&texts); // 同款 trim/去重/空格拼接
                if let Some(t) = joined {
                    table.cells[ci].text = Some(t);
                    filled_any = true;
                }
            }
            // 回捞成功 → 用结构 tokens 重建 HTML（td 序 ⇄ (row,col) 网格，
            // 镜像上游 collect_cell_texts_for_tokens 的映射规则）。
            if filled_any {
                regenerate_table_html(table);
            }
        }
    }
}

/// 按 `structure_tokens` + 当前 cells 重新生成 `html_structure`。
/// 无 tokens / 无网格信息时不动 HTML（保守：宁缺勿错）。
fn regenerate_table_html(table: &mut TableResult) {
    let Some(tokens) = table.structure_tokens.clone() else { return };
    let grid: Vec<CellGridInfo> = parse_cell_grid_info(&tokens);
    let mut grid_to_cell: std::collections::HashMap<(usize, usize), usize> =
        std::collections::HashMap::new();
    let mut has_grid = false;
    for (idx, c) in table.cells.iter().enumerate() {
        if let (Some(r), Some(col)) = (c.row, c.col) {
            grid_to_cell.insert((r, col), idx);
            has_grid = true;
        }
    }
    let cell_texts: Vec<Option<String>> = if has_grid {
        grid.iter()
            .map(|gi| {
                grid_to_cell
                    .get(&(gi.row, gi.col))
                    .and_then(|&i| table.cells.get(i))
                    .and_then(|c| c.text.clone())
            })
            .collect()
    } else {
        // 上游同款下标兜底：td 序 = cells 序
        (0..grid.len())
            .map(|i| table.cells.get(i).and_then(|c| c.text.clone()))
            .collect()
    };
    table.html_structure = Some(wrap_table_html_with_content(&tokens, &cell_texts));
    table.cell_texts = Some(cell_texts);
}

/// 裁剪并夹到页图范围内；退化框（宽或高 <2px、完全出画）→ None。
fn crop_clamped(img: &RgbImage, bbox: &BoundingBox) -> Option<RgbImage> {
    let (w, h) = (img.width() as f32, img.height() as f32);
    let x0 = bbox.x_min().clamp(0.0, w);
    let y0 = bbox.y_min().clamp(0.0, h);
    let x1 = bbox.x_max().clamp(0.0, w);
    let y1 = bbox.y_max().clamp(0.0, h);
    if x1 - x0 < 2.0 || y1 - y0 < 2.0 {
        return None;
    }
    BBoxCrop::crop_bounding_box(img, &BoundingBox::from_coords(x0, y0, x1, y1)).ok()
}

/// 同类告警只打一次（防逐页/逐格刷屏）。
fn warn_once(kind: &str, msg: &str) {
    static SEEN: OnceLock<Mutex<std::collections::HashSet<String>>> = OnceLock::new();
    let seen = SEEN.get_or_init(|| Mutex::new(std::collections::HashSet::new()));
    let mut g = seen.lock().unwrap_or_else(|p| p.into_inner());
    if g.insert(kind.to_string()) {
        eprintln!("[anydoc-ocr] 警告：{msg}（同类告警只显示一次）");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use oar_ocr::domain::structure::TableType;

    #[test]
    fn seal_default_on_with_no_var_and_legacy_alias_stays_on() {
        // #10b 默认翻转的三条真值：都不设=开；只设老变量=开（别名，绝不反向）；
        // 设 NO_ 变量=关（不限值）。
        assert!(seal_on_from(None, None), "默认必须开");
        assert!(seal_on_from(None, Some("1")), "老脚本 ANYDOC_SEAL_OCR=1 仍为开");
        assert!(seal_on_from(None, Some("")), "老变量空串历史上即开启，保持开");
        for off in ["1", "0", "", "anything"] {
            assert!(!seal_on_from(Some(off), None), "ANYDOC_NO_SEAL_OCR={off:?} 应为关");
        }
        // 两个同时给出：以关闭为准（NO_ 优先），不因老变量把默认又翻回开。
        assert!(!seal_on_from(Some("1"), Some("1")));
    }

    #[test]
    fn candidates_prefer_local_mineru_name_then_registry() {
        let dir = std::env::temp_dir().join(format!("anydoc_sealcand_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("seal_ppocrv4_det.onnx"), b"x").unwrap();
        let c = seal_model_candidates(dir.to_str().unwrap(), "pp-ocrv4_mobile_seal_det.onnx");
        assert_eq!(c.len(), 2, "本地 mineru 名 + 裸注册名");
        assert!(c[0].0.ends_with("seal_ppocrv4_det.onnx") && c[0].1);
        assert_eq!(c[1].0, "pp-ocrv4_mobile_seal_det.onnx");
        assert!(!c[1].1);
        // 空目录 → 只有裸名
        let c = seal_model_candidates("", "pp-ocrv4_mobile_seal_det.onnx");
        assert_eq!(c.len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn crop_clamped_rejects_degenerate_and_clips_overflow() {
        let img = RgbImage::from_pixel(100, 100, image::Rgb([255, 255, 255]));
        assert!(crop_clamped(&img, &BoundingBox::from_coords(10., 10., 11., 90.)).is_none());
        assert!(crop_clamped(&img, &BoundingBox::from_coords(-5., -5., 50., 60.)).is_some());
        assert!(crop_clamped(&img, &BoundingBox::from_coords(200., 200., 300., 300.)).is_none());
    }

    // ── #4 HTML 重建内核（回捞后必须把文本落到正确的 <td>）──

    /// 2×2 结构 token（`<td></td>` × 4）。
    fn tokens_2x2() -> Vec<String> {
        ["<tr>", "<td></td>", "<td></td>", "</tr>", "<tr>", "<td></td>", "<td></td>", "</tr>"]
            .map(String::from)
            .to_vec()
    }

    fn cell(row: usize, col: usize, text: Option<String>) -> oar_ocr::domain::structure::TableCell {
        let mut c = oar_ocr::domain::structure::TableCell::new(
            BoundingBox::from_coords((col * 50) as f32, (row * 30) as f32, (col * 50 + 50) as f32, (row * 30 + 30) as f32),
            0.9,
        );
        c.row = Some(row);
        c.col = Some(col);
        c.text = text;
        c
    }

    /// 网格 (row,col) → td 序映射：cells 顺序与 td 序**不一致**时仍各归其位，
    /// 空格保持空、回捞文本落进自己那一格。
    #[test]
    fn html_regen_uses_grid_not_cells_order() {
        let mut t = TableResult::new(BoundingBox::from_coords(0., 0., 100., 60.), TableType::Wired);
        t.structure_tokens = Some(tokens_2x2());
        // td 序：(0,0) (0,1) (1,0) (1,1)；cells 故意乱序且首格为空
        t.cells = vec![
            cell(1, 0, Some("左下".into())),
            cell(0, 0, None),
            cell(1, 1, Some("右下".into())),
            cell(0, 1, Some("右上".into())),
        ];
        regenerate_table_html(&mut t);
        let html = t.html_structure.clone().expect("应重建 HTML");
        let texts = t.cell_texts.as_ref().expect("应同步 cell_texts");
        assert_eq!(
            texts.iter().map(|t| t.as_deref().unwrap_or("")).collect::<Vec<_>>(),
            vec!["", "右上", "左下", "右下"],
            "网格映射错乱: {html}"
        );
        // HTML 内 td 内容顺序与 cell_texts 一致
        let filled: Vec<&str> = html
            .split("<td>")
            .skip(1)
            .map(|s| s.split("</td>").next().unwrap_or(""))
            .collect();
        assert_eq!(filled, vec!["", "右上", "左下", "右下"], "td 内容错位: {html}");
    }

    /// 无 structure_tokens（E2E 档/结构识别失败）→ **不动** HTML，保守跳过。
    #[test]
    fn html_regen_without_tokens_is_noop() {
        let mut t = TableResult::new(BoundingBox::from_coords(0., 0., 100., 60.), TableType::Wired);
        t.html_structure = Some("<html><body><table><tr><td>a</td></tr></table></body></html>".into());
        t.cells = vec![cell(0, 0, Some("回捞".into()))];
        let before = t.html_structure.clone();
        regenerate_table_html(&mut t);
        assert_eq!(t.html_structure, before, "无 tokens 不得改写 HTML");
        assert!(t.cell_texts.is_none());
    }

    /// cells 缺 row/col（仅检测框、无网格元数据）→ 上游同款**下标兜底**：
    /// td 序 = cells 序；td 数多于 cells 时尾部为空。
    #[test]
    fn html_regen_falls_back_to_index_order() {
        let mut t = TableResult::new(BoundingBox::from_coords(0., 0., 100., 60.), TableType::Wired);
        t.structure_tokens = Some(tokens_2x2());
        let mut c0 = cell(0, 0, Some("甲".into()));
        c0.row = None;
        c0.col = None;
        let mut c1 = cell(0, 1, Some("乙".into()));
        c1.row = None;
        c1.col = None;
        t.cells = vec![c0, c1];
        regenerate_table_html(&mut t);
        let texts = t.cell_texts.as_ref().expect("应产出 cell_texts");
        assert_eq!(texts.len(), 4, "td 数应 = 网格单元数");
        assert_eq!(
            texts.iter().map(|t| t.as_deref().unwrap_or("")).collect::<Vec<_>>(),
            vec!["甲", "乙", "", ""]
        );
    }

    /// 合并单元格（colspan/rowspan）网格：文本按 (row,col) 落位，被跨列占据的
    /// 后续格不误填——回捞最常见的错位事故形态。token 用上游字典的真实切分
    /// 形态（属性独立成 token、值带引号），非手写紧凑 HTML。
    #[test]
    fn html_regen_honours_span_tokens() {
        let tokens: Vec<String> = [
            "<tr>",
            "<td",
            " colspan=\"2\"",
            ">",
            "</td>",
            "</tr>",
            "<tr>",
            "<td></td>",
            "<td></td>",
            "</tr>",
        ]
        .map(String::from)
        .to_vec();
        let grid = parse_cell_grid_info(&tokens);
        assert_eq!(grid.len(), 3, "tokens 应有 3 个 td");
        assert_eq!((grid[0].row, grid[0].col, grid[0].col_span), (0, 0, 2), "跨列头解析异常");
        let mut t = TableResult::new(BoundingBox::from_coords(0., 0., 100., 60.), TableType::Wired);
        t.structure_tokens = Some(tokens);
        // cells 顺序刻意打乱：td 序 = (0,0) 跨列头 → (1,0) → (1,1)
        t.cells = vec![
            cell(1, 1, Some("回捞2".into())),
            cell(0, 0, Some("跨列头".into())),
            cell(1, 0, Some("回捞1".into())),
        ];
        regenerate_table_html(&mut t);
        let texts = t.cell_texts.as_ref().expect("应产出 cell_texts");
        assert_eq!(
            texts.iter().map(|t| t.as_deref().unwrap_or("")).collect::<Vec<_>>(),
            vec!["跨列头", "回捞1", "回捞2"],
            "跨列网格映射错乱"
        );
    }
}
