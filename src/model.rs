//! 表抽出の内部データモデルと JSON 出力スキーマ
//! 座標は top-down に統一する

use serde::{Deserialize, Serialize};

/// 矩形領域
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct BBox {
    #[serde(rename = "left")]
    pub x0: f64,
    pub top: f64,
    #[serde(rename = "right")]
    pub x1: f64,
    pub bottom: f64,
}

/// 1文字とその外接矩形
#[derive(Debug, Clone, Copy)]
pub struct Glyph {
    pub ch: char,
    pub bbox: BBox,
}

impl BBox {
    pub fn width(&self) -> f64 {
        self.x1 - self.x0
    }
    pub fn height(&self) -> f64 {
        self.bottom - self.top
    }
    pub fn cx(&self) -> f64 {
        (self.x0 + self.x1) / 2.0
    }
    pub fn cy(&self) -> f64 {
        (self.top + self.bottom) / 2.0
    }
    /// inner の中心が self に入るか
    pub fn contains_center(&self, inner: &BBox) -> bool {
        let cx = inner.cx();
        let cy = inner.cy();
        cx >= self.x0 && cx < self.x1 && cy >= self.top && cy < self.bottom
    }
}

/// 辺の向き
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Orientation {
    Horizontal,
    Vertical,
}

/// 罫線や文字整列から導く辺
#[derive(Debug, Clone, Copy)]
pub struct Edge {
    pub x0: f64,
    pub top: f64,
    pub x1: f64,
    pub bottom: f64,
    pub orientation: Orientation,
}

impl Edge {
    /// 延伸方向の寸法
    pub fn length(&self) -> f64 {
        match self.orientation {
            Orientation::Horizontal => self.x1 - self.x0,
            Orientation::Vertical => self.bottom - self.top,
        }
    }
}

/// 表のセル
#[derive(Debug, Clone, Serialize)]
pub struct Cell {
    pub text: String,
    pub bbox: BBox,
}

/// 検出された1つの表
#[derive(Debug, Clone, Serialize)]
pub struct Table {
    /// 検出方式
    pub extraction_method: &'static str,
    pub bbox: BBox,
    pub n_rows: usize,
    pub n_cols: usize,
    /// 行×セルの二次元配列
    pub data: Vec<Vec<Cell>>,
}

/// 1ページ分の出力
#[derive(Debug, Clone, Serialize)]
pub struct Page {
    pub page_number: usize,
    pub width: f64,
    pub height: f64,
    pub tables: Vec<Table>,
}

/// ドキュメント全体の出力 JSON のルート
#[derive(Debug, Clone, Serialize)]
pub struct Document {
    pub source: String,
    pub pages: Vec<Page>,
}
