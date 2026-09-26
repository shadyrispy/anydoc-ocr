//! 安全/资源限制（审计 #8/#9，口径对齐 MinerU 4.0）：
//!
//! MinerU 的思路是**入口闸 + 像素钳 + 运行短路**三层，全部有明确常量，
//! 超限显式报错（HTTP 413 `upload_size_mismatch`），绝不静默截断：
//! - 入口：`_MAX_FILE_SIZE_BYTES_DEFAULT = 200 MiB`（单文件上传）、
//!   `max_pages_per_file = 1000`（`mineru/parser/api_server.py`）；
//! - 像素：`DEFAULT_MAX_RENDER_EDGE = 3500`（docvortex `pdf/raster.py`，
//!   页再大、dpi 再高，长边钳到 3500px 后整体降 scale）；
//! - 运行：单页原生字符 > `65535` 放弃文字层抽取直转 OCR 路径
//!   （`pdf/text/native.py::MAX_NATIVE_TEXT_CHARS_PER_PAGE`，防解析卡死）。
//!
//! 本模块对应落三层（dpi 数值区间是我们独有的，MinerU 不暴露该旋钮）：
//! - [`MAX_INPUT_BYTES`]：stdin / 单文档输入字节闸（`ANYDOC_MAX_INPUT_BYTES` 覆盖）；
//! - [`validate_dpi`]：`--dpi` 拒绝非正/NaN 并限定 [`DPI_MIN`]=[`DPI_MAX`] 区间；
//! - [`DEFAULT_RENDER_EDGE_CAP`]：PDFium/OFD 渲染长边上限（`ANYDOC_RENDER_EDGE_CAP` 覆盖）；
//! - [`MAX_NATIVE_TEXT_CHARS_PER_PAGE`]：单页文字层字符短路（`ANYDOC_NATIVE_TEXT_CHARS` 覆盖）。
//!
//! 环境变量解析口径与 MinerU `_positive_int_env` 一致：**非法值（不可解析/≤0）
//! 回落默认**，只有显式合法值才生效；`0` 一律视为非法（闸不提供"关闭"语义，
//! 想放宽请给显式大值）。

use crate::error::{ConvertError, ErrorKind, Result, Stage};

/// stdin / 单文档输入字节上限（对齐 MinerU 上传档 200 MiB）。
/// `ANYDOC_MAX_INPUT_BYTES`（字节，正整数）覆盖。
pub(crate) const MAX_INPUT_BYTES: u64 = 200 * 1024 * 1024;

/// 渲染长边上限（px），对齐 docvortex `DEFAULT_MAX_RENDER_EDGE=3500`：
/// `scale = min(dpi/72, cap/长边)` 整体降 scale。`ANYDOC_RENDER_EDGE_CAP` 覆盖
/// （非法值回落 3500；旧语义"未设置=不钳"已按 #9 升为默认钳位）。
pub(crate) const DEFAULT_RENDER_EDGE_CAP: f32 = 3500.0;

/// 单页原生文字层字符上限，对齐 MinerU `MAX_NATIVE_TEXT_CHARS_PER_PAGE`
/// （65535 = u16::MAX，其动机是防超大文字层页把抽取环节卡死；本仓动机同：
/// 拆行/阅读序/表格启发式全部随整页 items 规模放大耗时）。超限页跳过
/// 文字层直判 OCR 缺页。`ANYDOC_NATIVE_TEXT_CHARS` 覆盖。
pub(crate) const MAX_NATIVE_TEXT_CHARS_PER_PAGE: usize = 65_535;

/// 单文档页数上限（对齐 MinerU `UsageLimits.max_pages_per_file = 1000`）。
/// `ANYDOC_MAX_PAGES`（正整数）覆盖。
pub(crate) const MAX_PAGES: u64 = 1000;

/// dpi 合法区间。下限 50：实测 80 起脚注/小字开始漏检（README「已知限制」），
/// 50 以下 OCR 基本失效，不如显式拒绝；上限 400：像素闸之外再加一道防误配
/// （400dpi 单页像素已是 100dpi 的 16 倍）。MinerU 不给用户开旋钮（恒 200），
/// 我们的 dpi 是校准出的性能杠杆，故不学它禁旋钮，学它"非法即拒"。
pub(crate) const DPI_MIN: f32 = 50.0;
pub(crate) const DPI_MAX: f32 = 400.0;

/// `--dpi` 校验（#9）：NaN/Inf / 超出 [`DPI_MIN`]=[`DPI_MAX`] → 显式 Err。
/// 不回静默钳值：dpi 是用户显式输入（区别于 MinerU 的环境值→默认语义），
/// 报错比按错值跑完全程便宜。库路径同样经过（convert 入口统一校验）。
/// 公开面经 `lib.rs::validate_render_dpi` 导出（CLI 早失败用）。
pub(crate) fn validate_dpi(dpi: f32) -> Result<()> {
    if !dpi.is_finite() {
        return Err(ConvertError::new(
            ErrorKind::Unsupported,
            Stage::Convert,
            format!("渲染 DPI 非法（NaN/Inf）: {dpi}，允许区间 [{DPI_MIN}, {DPI_MAX}]"),
        ));
    }
    if dpi < DPI_MIN || dpi > DPI_MAX {
        return Err(ConvertError::new(
            ErrorKind::Unsupported,
            Stage::Convert,
            format!("渲染 DPI 越界: {dpi}，允许区间 [{DPI_MIN}, {DPI_MAX}]（50 以下小字漏检，400 以上纯内存浪费）"),
        ));
    }
    Ok(())
}

/// 正整数环境变量（MinerU `_positive_int_env` 同语义）：未设置/不可解析/≤0
/// → default。
pub(crate) fn positive_env(name: &str, default: u64) -> u64 {
    positive_from(std::env::var(name).ok().as_deref(), default)
}

/// 纯函数内核（可单测，不触全局 env——仓内惯例，见 hybrid_disabled_from）。
fn positive_from(v: Option<&str>, default: u64) -> u64 {
    v.and_then(|s| s.trim().parse::<u64>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(default)
}

/// 正浮点环境变量（同上，用于像素钳位）。
pub(crate) fn positive_f32_env(name: &str, default: f32) -> f32 {
    positive_f32_from(std::env::var(name).ok().as_deref(), default)
}

fn positive_f32_from(v: Option<&str>, default: f32) -> f32 {
    v.and_then(|s| s.trim().parse::<f32>().ok())
        .filter(|n| n.is_finite() && *n > 0.0)
        .unwrap_or(default)
}

/// 当前生效的输入字节上限（`ANYDOC_MAX_INPUT_BYTES` 覆盖，非法值回落默认）。
pub(crate) fn max_input_bytes() -> u64 {
    positive_env("ANYDOC_MAX_INPUT_BYTES", MAX_INPUT_BYTES)
}

/// 输入字节闸（#8）：`size > limit` → `ResourceLimit` Err（对齐 MinerU
/// 413 语义：拒绝并说明，不截断不误读）。
pub(crate) fn check_input_size(path: &std::path::Path, size: u64) -> Result<()> {
    check_size_from(path, size, max_input_bytes())
}

/// 有界读 stdin（#8）：累计超过当前上限立即断读并 `ResourceLimit` Err——
/// 不再 `read_to_end` 无界占用内存（审计原语：管道投喂大文件可打满 RAM）。
/// 读满且未超限才返回缓冲。IO 错误原样上抛。
pub(crate) fn read_stdin_bounded() -> Result<Vec<u8>> {
    use std::io::Read;
    let limit = max_input_bytes() as usize;
    let mut buf = Vec::with_capacity(limit.div_ceil(16).min(1 << 20));
    let mut chunk = [0u8; 64 * 1024];
    let stdin = std::io::stdin();
    let mut r = stdin.lock();
    loop {
        let n = r.read(&mut chunk).map_err(|e| {
            ConvertError::new(ErrorKind::Io, Stage::Convert, format!("读取 stdin 失败: {e}"))
        })?;
        if n == 0 {
            return Ok(buf);
        }
        if buf.len() + n > limit {
            return Err(ConvertError::new(
                ErrorKind::ResourceLimit,
                Stage::Convert,
                format!(
                    "输入超出大小上限: <stdin> > {limit} bytes（ANYDOC_MAX_INPUT_BYTES 可调）"
                ),
            ));
        }
        buf.extend_from_slice(&chunk[..n]);
    }
}

fn check_size_from(path: &std::path::Path, size: u64, limit: u64) -> Result<()> {
    if size > limit {
        return Err(ConvertError::new(
            ErrorKind::ResourceLimit,
            Stage::Convert,
            format!(
                "输入超出大小上限: {} = {size} bytes > {limit} bytes（ANYDOC_MAX_INPUT_BYTES 可调）",
                path.display()
            ),
        ));
    }
    Ok(())
}

/// 文件大小闸：stat 失败交回调用方按 Io 处理；0/未知大小放行（管道等）。
pub(crate) fn check_file_size(path: &std::path::Path) -> Result<()> {
    match std::fs::metadata(path) {
        Ok(md) if md.is_file() => check_input_size(path, md.len()),
        _ => Ok(()),
    }
}

/// 当前生效的页数上限（`ANYDOC_MAX_PAGES` 覆盖，非法值回落 1000）。
pub(crate) fn max_pages() -> u64 {
    positive_env("ANYDOC_MAX_PAGES", MAX_PAGES)
}

/// 页数闸（对齐 MinerU `max_pages_per_file`）：超限 → `ResourceLimit` 显式拒绝。
pub(crate) fn check_page_count(path: &std::path::Path, pages: u64) -> Result<()> {
    check_page_count_from(path, pages, max_pages())
}

fn check_page_count_from(path: &std::path::Path, pages: u64, limit: u64) -> Result<()> {
    if pages > limit {
        return Err(ConvertError::new(
            ErrorKind::ResourceLimit,
            Stage::Convert,
            format!(
                "页数超出上限: {} = {pages} 页 > {limit} 页（ANYDOC_MAX_PAGES 可调）",
                path.display()
            ),
        ));
    }
    Ok(())
}

/// 渲染长边上限（当前生效值，env 覆盖或默认 3500）。
pub(crate) fn render_edge_cap() -> f32 {
    positive_f32_env("ANYDOC_RENDER_EDGE_CAP", DEFAULT_RENDER_EDGE_CAP)
}

/// dpi 缩放 + 长边钳位（docvortex `page_to_image` 同式）：
/// `scale = min(dpi/72, cap/长边)`。退化页尺寸（≤0/NaN）不钳。
pub(crate) fn render_scale(dpi: f32, page_long_edge: f32) -> f32 {
    render_scale_from(dpi, page_long_edge, render_edge_cap())
}

fn render_scale_from(dpi: f32, page_long_edge: f32, cap: f32) -> f32 {
    let base = dpi / 72.0;
    if page_long_edge.is_finite() && page_long_edge > 0.0 && page_long_edge * base > cap {
        cap / page_long_edge
    } else {
        base
    }
}

/// OFD 侧等价钳位（同一 `render_scale` 语义，换 dpi 表达）：ofd-core 像素 =
/// mm/25.4×dpi，故把长边钳到 cap px 等价于 `eff_dpi = min(dpi, cap×25.4/长边mm)`。
/// 退化页尺寸（≤0/NaN）不钳。
pub(crate) fn effective_dpi_mm(dpi: f32, long_edge_mm: f64) -> f32 {
    effective_dpi_mm_from(dpi, long_edge_mm, render_edge_cap())
}

fn effective_dpi_mm_from(dpi: f32, long_edge_mm: f64, cap: f32) -> f32 {
    if long_edge_mm.is_finite() && long_edge_mm > 0.0 {
        let max_dpi = (cap as f64 * 25.4 / long_edge_mm) as f32;
        if dpi > max_dpi {
            return max_dpi;
        }
    }
    dpi
}

/// 单页字符短路阈值（当前生效值）。
pub(crate) fn native_text_char_cap() -> usize {
    positive_env("ANYDOC_NATIVE_TEXT_CHARS", MAX_NATIVE_TEXT_CHARS_PER_PAGE as u64) as usize
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dpi_validation() {
        assert!(validate_dpi(100.0).is_ok());
        assert!(validate_dpi(50.0).is_ok());
        assert!(validate_dpi(400.0).is_ok());
        for bad in [0.0, -1.0, 49.9, 400.1, f32::NAN, f32::INFINITY] {
            let e = validate_dpi(bad).unwrap_err();
            assert_eq!(e.kind, ErrorKind::Unsupported, "dpi={bad}");
        }
    }

    #[test]
    fn positive_env_semantics() {
        // 非法（未设置/不可解析 / 0 / 负）→ 默认；合法正整数 → 生效。
        assert_eq!(positive_from(None, 7), 7);
        assert_eq!(positive_from(Some("abc"), 7), 7);
        assert_eq!(positive_from(Some("0"), 7), 7);
        assert_eq!(positive_from(Some("-3"), 7), 7);
        assert_eq!(positive_from(Some(" 12 "), 7), 12);
        assert_eq!(positive_f32_from(None, 3500.0), 3500.0);
        assert_eq!(positive_f32_from(Some("nan"), 3500.0), 3500.0);
        assert_eq!(positive_f32_from(Some("0"), 3500.0), 3500.0);
        assert!((positive_f32_from(Some("2000"), 3500.0) - 2000.0).abs() < 1e-6);
    }

    #[test]
    fn edge_scale_clamps_like_docvortex() {
        let cap = DEFAULT_RENDER_EDGE_CAP;
        // A4 长边 ≈ 842pt：100dpi → scale 1.39，842×1.39 ≈ 1169px < 3500 → 不钳。
        let s = render_scale_from(100.0, 842.0, cap);
        assert!((s - 100.0 / 72.0).abs() < 1e-6);
        // 超大页（4000pt 长边）× 100dpi → 5556px > 3500 → 钳到 3500/4000。
        let s = render_scale_from(100.0, 4000.0, cap);
        assert!((s - 3500.0 / 4000.0).abs() < 1e-3);
        // 退化页尺寸（0/NaN）→ 不钳、不 panic。
        assert!((render_scale_from(100.0, 0.0, cap) - 100.0 / 72.0).abs() < 1e-6);
        assert!((render_scale_from(100.0, f32::NAN, cap) - 100.0 / 72.0).abs() < 1e-6);
    }

    #[test]
    fn size_gate_reports_resource_limit() {
        let p = std::path::Path::new("x.pdf");
        assert!(check_size_from(p, MAX_INPUT_BYTES, MAX_INPUT_BYTES).is_ok());
        let e = check_size_from(p, MAX_INPUT_BYTES + 1, MAX_INPUT_BYTES).unwrap_err();
        assert_eq!(e.kind, ErrorKind::ResourceLimit);
        assert_eq!(e.code(), "resourceLimit");
    }

    #[test]
    fn page_gate_reports_resource_limit() {
        let p = std::path::Path::new("x.pdf");
        // 边界：恰好等于上限放行（与字节闸同 `>` 语义）
        assert!(check_page_count_from(p, MAX_PAGES, MAX_PAGES).is_ok());
        let e = check_page_count_from(p, MAX_PAGES + 1, MAX_PAGES).unwrap_err();
        assert_eq!(e.kind, ErrorKind::ResourceLimit);
        assert_eq!(e.code(), "resourceLimit");
        assert!(e.to_string().contains("ANYDOC_MAX_PAGES"), "{e}");
    }
}
