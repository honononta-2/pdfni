//! テキスト埋め込みPDFから純粋な表を抽出しJSON可能な構造で返すライブラリ

#[cfg(feature = "reader")]
mod columns;
mod detect_hybrid;
mod detect_lattice;
mod detect_text;
#[cfg(feature = "reader")]
mod document;
mod edges_geom;
mod error;
mod extract;
#[cfg(feature = "reader")]
mod furniture;
mod model;
mod par;
#[cfg(feature = "reader")]
mod reader;
#[cfg(feature = "reader")]
mod text;
#[cfg(target_arch = "wasm32")]
mod wasm;

pub use detect_hybrid::HybridSettings;
pub use detect_lattice::LatticeSettings;
pub use detect_text::TextSettings;
#[cfg(feature = "reader")]
pub use document::{
    DocBlock, DocDoc, DocPage, TextBlock, TextBlockKind, TextBlockRole, assign_header_footer,
    assign_headings, assign_lists, build_blocks, document_to_markdown, extract_document_from_bytes,
};
pub use error::{Error, Result};
pub use extract::{
    DetectMode, EdgePart, ExtractOptions, GlyphPart, GraphicPart, PagePart, extract_from_parts,
};
pub use model::{BBox, Cell, Document, Orientation, Page, Table};
#[cfg(feature = "reader")]
pub use reader::{extract_from_bytes, extract_text_from_bytes, read_pages};
#[cfg(feature = "reader")]
pub use text::{
    TextChar, TextDoc, TextFont, TextItem, TextLine, TextPage, TextWord, build_text_items,
    build_text_lines,
};
