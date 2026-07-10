//! テキスト埋め込みPDFから純粋な表を抽出しJSON可能な構造で返すライブラリ

mod detect_hybrid;
mod detect_lattice;
mod detect_text;
mod edges_geom;
mod error;
mod extract;
mod model;
mod par;
#[cfg(feature = "reader-rs")]
mod reader_rs;
#[cfg(target_arch = "wasm32")]
mod wasm;

pub use detect_hybrid::HybridSettings;
pub use detect_lattice::LatticeSettings;
pub use detect_text::TextSettings;
pub use error::{Error, Result};
pub use extract::{
    DetectMode, EdgePart, ExtractOptions, GlyphPart, GraphicPart, PagePart, extract_from_parts,
};
pub use model::{BBox, Cell, Document, Orientation, Page, Table};
#[cfg(feature = "reader-rs")]
pub use reader_rs::{extract_from_bytes_rs, read_pages_rs};
