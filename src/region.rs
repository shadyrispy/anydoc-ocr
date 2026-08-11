//! 文本区域统一类型：替代全仓贯穿的 `(f32, f32, f32, f32, String)` 元组。
//!
//! 字段语义（与 `reading_order` 约定一致）：`x_min`/`x_max` 为区域水平范围，
//! `y_min`/`y_max` 为垂直范围（**越小越靠上**——PDF 调用侧已翻转、OFD 原生左上），
//! `text` 为区域文本。表网格通路接收的 `(x, y, w, h, text)` 块用
//! [`Region::from_top_left`] 转换后存入同一类型（`h = y_max - y_min`）。
//!
//! 整宽判定阈值集中于此，消除 `reading_order` 内 0.92/0.08 的重复魔法数。

/// 整宽判定：区域跨度须 > 此比例 × 页宽（剔除通栏正文长条）。
pub const FULL_WIDTH_THRESHOLD: f32 = 0.92;
/// 整宽判定：区域左缘须 < 此比例 × 页宽（贴近左页边）。
pub const EDGE_MARGIN: f32 = 0.08;

#[derive(Clone, Debug, PartialEq)]
pub struct Region {
    pub x_min: f32,
    pub x_max: f32,
    pub y_min: f32,
    pub y_max: f32,
    pub text: String,
}

impl Region {
    pub fn new(x_min: f32, x_max: f32, y_min: f32, y_max: f32, text: impl Into<String>) -> Self {
        Region {
            x_min,
            x_max,
            y_min,
            y_max,
            text: text.into(),
        }
    }

    /// 从左上角 + 宽高构造（表网格块 `(x, y, w, h)` 形式）。
    pub fn from_top_left(x: f32, y: f32, w: f32, h: f32, text: impl Into<String>) -> Self {
        Region {
            x_min: x,
            x_max: x + w,
            y_min: y,
            y_max: y + h,
            text: text.into(),
        }
    }

    pub fn width(&self) -> f32 {
        self.x_max - self.x_min
    }

    pub fn height(&self) -> f32 {
        self.y_max - self.y_min
    }

    pub fn center_x(&self) -> f32 {
        (self.x_min + self.x_max) / 2.0
    }

    /// 整宽判定：跨度 > 92% 页宽 且 左缘 < 8% 页宽 → 页眉/页脚/通栏标题。
    /// 与 `detect_column_split` 共用同一口径（同一常量）。
    pub fn is_full_width(&self, page_w: f32) -> bool {
        self.width() > FULL_WIDTH_THRESHOLD * page_w && self.x_min < EDGE_MARGIN * page_w
    }

    /// 一组区域的最大 `x_max`（页宽估计）。
    pub fn page_w(regions: &[Region]) -> f32 {
        regions.iter().map(|r| r.x_max).fold(0.0_f32, f32::max)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_top_left_expands_to_box() {
        let r = Region::from_top_left(10.0, 20.0, 30.0, 40.0, "x");
        assert_eq!(r.x_min, 10.0);
        assert_eq!(r.x_max, 40.0);
        assert_eq!(r.y_min, 20.0);
        assert_eq!(r.y_max, 60.0);
        assert_eq!(r.width(), 30.0);
        assert_eq!(r.height(), 40.0);
    }

    #[test]
    fn is_full_width_matches_threshold() {
        // 页宽 1000：整宽须跨度 >920 且 左缘 <80
        assert!(Region::new(0.0, 1000.0, 5.0, 15.0, "hdr").is_full_width(1000.0));
        // 左列长条目：跨度 400（<920）→ 非整宽
        assert!(!Region::new(50.0, 450.0, 100.0, 110.0, "L").is_full_width(1000.0));
        // 左缘贴边但跨度不足 → 非整宽
        assert!(!Region::new(0.0, 800.0, 100.0, 110.0, "M").is_full_width(1000.0));
        // 跨度够但左缘不贴边 → 非整宽
        assert!(!Region::new(100.0, 1020.0, 100.0, 110.0, "R").is_full_width(1000.0));
    }

    #[test]
    fn page_w_is_max_x_max() {
        let rs = vec![
            Region::new(0.0, 500.0, 0.0, 10.0, "a"),
            Region::new(0.0, 800.0, 0.0, 10.0, "b"),
            Region::new(0.0, 300.0, 0.0, 10.0, "c"),
        ];
        assert_eq!(Region::page_w(&rs), 800.0);
    }
}
