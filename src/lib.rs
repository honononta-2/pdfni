//! テキスト埋め込みPDFから純粋な表を抽出しJSON可能な構造で返すライブラリ

mod bidi;
#[cfg(feature = "document")]
mod columns;
mod detect_hybrid;
mod detect_lattice;
mod detect_text;
#[cfg(feature = "document")]
mod document;
mod edges_geom;
mod error;
mod extract;
#[cfg(feature = "document")]
mod furniture;
mod model;
mod par;
#[cfg(feature = "reader")]
mod reader;
#[cfg(feature = "text")]
mod text;
#[cfg(target_arch = "wasm32")]
mod wasm;

pub use bidi::{BidiText, apply_bidi, bidi};
pub use detect_hybrid::HybridSettings;
pub use detect_lattice::LatticeSettings;
pub use detect_text::TextSettings;
#[cfg(feature = "document")]
pub use document::{
    DocBlock, DocDoc, DocPage, TextBlock, TextBlockKind, TextBlockRole, assign_header_footer,
    assign_headings, assign_lists, build_blocks, extract_document_from_bytes,
    extract_markdown_from_bytes,
};
pub use error::{Error, Result};
pub use extract::{
    DetectMode, EdgePart, ExtractOptions, GlyphPart, GraphicPart, PagePart, ReaderSettings,
    extract_from_parts,
};
pub use model::{BBox, Cell, Document, Orientation, Page, Table};
#[cfg(feature = "reader")]
pub use reader::{
    PartsView, ReaderDump, ReaderDumpPage, TextView, extract_from_bytes, extract_text_from_bytes,
    extract_text_from_bytes_with, read_pages, read_pages_full, read_pages_full_with, read_pages_with,
};
#[cfg(feature = "text")]
pub use text::{
    TextChar, TextDoc, TextFont, TextItem, TextLine, TextPage, TextWord, build_text_items,
    build_text_items_with, build_text_lines, build_text_lines_with,
};
