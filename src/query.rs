//! ページ・矩形による抽出範囲の絞り込み

use crate::extract::PagePart;
use crate::model::Orientation;

/// 抽出範囲の絞り込み条件
#[derive(Debug, Clone, Default)]
pub struct ExtractQuery {
    pub pages: Option<PageSelection>,
    pub region: Option<Region>,
}

/// 1始まりのページ選択
#[derive(Debug, Clone, PartialEq)]
pub struct PageSelection {
    ranges: Vec<(u32, u32)>,
}

impl PageSelection {
    pub fn contains(&self, page_number: u32) -> bool {
        self.ranges
            .iter()
            .any(|&(a, b)| a <= page_number && page_number <= b)
    }
}

impl std::str::FromStr for PageSelection {
    type Err = crate::error::Error;

    /// `1,3-5,7` 形式
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let err = || crate::error::Error::InvalidPageSelection(s.to_string());
        let mut ranges = Vec::new();
        for token in s.split(',') {
            let (a, b) = match token.split_once('-') {
                Some((a, b)) => (a.trim(), b.trim()),
                None => (token.trim(), token.trim()),
            };
            let a: u32 = a.parse().map_err(|_| err())?;
            let b: u32 = b.parse().map_err(|_| err())?;
            if a == 0 || b < a {
                return Err(err());
            }
            ranges.push((a, b));
        }
        Ok(Self { ranges })
    }
}

/// top-down 表示座標の矩形
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Region {
    pub left: f64,
    pub top: f64,
    pub right: f64,
    pub bottom: f64,
}

impl Region {
    pub fn from_ltrb(left: f64, top: f64, right: f64, bottom: f64) -> Self {
        Self {
            left,
            top,
            right,
            bottom,
        }
    }

    pub fn from_ltwh(left: f64, top: f64, width: f64, height: f64) -> Self {
        Self {
            left,
            top,
            right: left + width,
            bottom: top + height,
        }
    }

    pub fn contains_point(&self, x: f64, y: f64) -> bool {
        self.left <= x && x <= self.right && self.top <= y && y <= self.bottom
    }
}

/// 矩形に掛かる parts だけ残す
///
/// グリフは中心点で残否を決め 罫線・図形は矩形との交差部分に切り詰める
/// 図形の curve_len は切り詰め前の値のまま
pub fn retain_parts_in_region(page: &mut PagePart, region: &Region) {
    let r = to_norm_space(region, page.norm_rotate, page.width, page.height);
    page.glyphs.retain(|g| {
        r.contains_point((g.left + g.right) / 2.0, (g.top + g.bottom) / 2.0)
    });
    page.edges.retain_mut(|e| {
        let (left, top, right, bottom) = (
            e.left.max(r.left),
            e.top.max(r.top),
            e.right.min(r.right),
            e.bottom.min(r.bottom),
        );
        if left > right || top > bottom {
            return false;
        }
        let long_enough = match e.orientation {
            Orientation::Horizontal => right > left,
            Orientation::Vertical => bottom > top,
        };
        if !long_enough {
            return false;
        }
        (e.left, e.top, e.right, e.bottom) = (left, top, right, bottom);
        true
    });
    page.graphics.retain_mut(|g| {
        let (left, top, right, bottom) = (
            g.left.max(r.left),
            g.top.max(r.top),
            g.right.min(r.right),
            g.bottom.min(r.bottom),
        );
        if left > right || top > bottom {
            return false;
        }
        (g.left, g.top, g.right, g.bottom) = (left, top, right, bottom);
        true
    });
}

/// 表示座標の矩形を norm_rotate 適用後の座標系へ変換する
///
/// w h は正規化後のページ寸法。extract の denorm_box の逆変換
fn to_norm_space(region: &Region, rot: i32, w: f64, h: f64) -> Region {
    let &Region {
        left,
        top,
        right,
        bottom,
    } = region;
    match rot {
        90 => Region {
            left: w - bottom,
            top: left,
            right: w - top,
            bottom: right,
        },
        180 => Region {
            left: w - right,
            top: h - bottom,
            right: w - left,
            bottom: h - top,
        },
        270 => Region {
            left: top,
            top: h - right,
            right: bottom,
            bottom: h - left,
        },
        _ => *region,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extract::{EdgePart, GlyphPart, GraphicPart};

    #[test]
    fn page_selection_parses_singles_ranges_and_mixes() {
        let sel: PageSelection = "1".parse().unwrap();
        assert!(sel.contains(1));
        assert!(!sel.contains(2));

        let sel: PageSelection = "1,3-5,7".parse().unwrap();
        for p in [1, 3, 4, 5, 7] {
            assert!(sel.contains(p));
        }
        for p in [2, 6, 8] {
            assert!(!sel.contains(p));
        }

        let sel: PageSelection = " 2 , 4 - 6 ".parse().unwrap();
        assert!(sel.contains(2));
        assert!(sel.contains(5));
    }

    #[test]
    fn page_selection_rejects_invalid_input() {
        for s in ["", "0", "a", "5-3", "1--2", "-3", "1,", "1-"] {
            assert!(s.parse::<PageSelection>().is_err(), "{s}");
        }
    }

    #[test]
    fn region_ltwh_matches_ltrb() {
        assert_eq!(
            Region::from_ltwh(10.0, 20.0, 30.0, 40.0),
            Region::from_ltrb(10.0, 20.0, 40.0, 60.0)
        );
    }

    fn glyph(ch: char, left: f64, top: f64, right: f64, bottom: f64) -> GlyphPart {
        GlyphPart {
            ch,
            left,
            right,
            top,
            bottom,
            font_size: None,
            upright: true,
            rot: 0,
        }
    }

    fn edge(orientation: Orientation, left: f64, top: f64, right: f64, bottom: f64) -> EdgePart {
        EdgePart {
            orientation,
            left,
            right,
            top,
            bottom,
        }
    }

    fn page(glyphs: Vec<GlyphPart>, edges: Vec<EdgePart>, graphics: Vec<GraphicPart>) -> PagePart {
        PagePart {
            width: 100.0,
            height: 200.0,
            glyphs,
            edges,
            norm_rotate: 0,
            graphics,
        }
    }

    #[test]
    fn retain_parts_keeps_glyphs_by_center() {
        let mut p = page(
            vec![
                glyph('A', 0.0, 0.0, 10.0, 10.0),
                glyph('B', 40.0, 40.0, 50.0, 50.0),
                glyph('C', 80.0, 80.0, 90.0, 90.0),
            ],
            Vec::new(),
            Vec::new(),
        );
        retain_parts_in_region(&mut p, &Region::from_ltrb(30.0, 30.0, 60.0, 60.0));
        assert_eq!(p.glyphs.len(), 1);
        assert_eq!(p.glyphs[0].ch, 'B');
    }

    #[test]
    fn retain_parts_clips_edges_and_drops_outside() {
        let mut p = page(
            Vec::new(),
            vec![
                edge(Orientation::Horizontal, 0.0, 50.0, 100.0, 50.0),
                edge(Orientation::Vertical, 40.0, 0.0, 40.0, 200.0),
                edge(Orientation::Horizontal, 0.0, 150.0, 100.0, 150.0),
                // 線方向では矩形に1点でしか触れない
                edge(Orientation::Horizontal, 0.0, 50.0, 30.0, 50.0),
            ],
            Vec::new(),
        );
        retain_parts_in_region(&mut p, &Region::from_ltrb(30.0, 30.0, 60.0, 60.0));
        assert_eq!(p.edges.len(), 2);
        let h = &p.edges[0];
        assert_eq!((h.left, h.right, h.top, h.bottom), (30.0, 60.0, 50.0, 50.0));
        let v = &p.edges[1];
        assert_eq!((v.left, v.right, v.top, v.bottom), (40.0, 40.0, 30.0, 60.0));
    }

    #[test]
    fn retain_parts_clips_graphics_bbox() {
        let mut p = page(
            Vec::new(),
            Vec::new(),
            vec![
                GraphicPart {
                    left: 0.0,
                    top: 0.0,
                    right: 40.0,
                    bottom: 40.0,
                    curve_len: 7.0,
                },
                GraphicPart {
                    left: 70.0,
                    top: 70.0,
                    right: 90.0,
                    bottom: 90.0,
                    curve_len: 0.0,
                },
            ],
        );
        retain_parts_in_region(&mut p, &Region::from_ltrb(30.0, 30.0, 60.0, 60.0));
        assert_eq!(p.graphics.len(), 1);
        let g = &p.graphics[0];
        assert_eq!((g.left, g.top, g.right, g.bottom), (30.0, 30.0, 40.0, 40.0));
        assert_eq!(g.curve_len, 7.0);
    }

    #[test]
    fn to_norm_space_inverts_denorm_box() {
        // 正規化後寸法 w=100 h=200。extract::denorm_box の各分岐と往復で一致する値
        let display = Region::from_ltrb(20.0, 70.0, 40.0, 90.0);
        assert_eq!(
            to_norm_space(&display, 90, 100.0, 200.0),
            Region::from_ltrb(10.0, 20.0, 30.0, 40.0)
        );
        let display = Region::from_ltrb(70.0, 160.0, 90.0, 180.0);
        assert_eq!(
            to_norm_space(&display, 180, 100.0, 200.0),
            Region::from_ltrb(10.0, 20.0, 30.0, 40.0)
        );
        let display = Region::from_ltrb(160.0, 10.0, 180.0, 30.0);
        assert_eq!(
            to_norm_space(&display, 270, 100.0, 200.0),
            Region::from_ltrb(10.0, 20.0, 30.0, 40.0)
        );
        let display = Region::from_ltrb(10.0, 20.0, 30.0, 40.0);
        assert_eq!(to_norm_space(&display, 0, 100.0, 200.0), display);
    }
}
