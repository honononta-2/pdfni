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
#[cfg(all(not(target_arch = "wasm32"), feature = "ffi"))]
mod ffi;
#[cfg(feature = "document")]
mod furniture;
mod model;
mod par;
mod query;
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
    ContentBlock, ContentLine, ContentPage, ContentText, DocBlock, DocContent, DocDoc, DocPage,
    TextBlock, TextBlockKind, TextBlockRole, assign_header_footer, assign_headings, assign_lists,
    build_blocks, extract_content_from_bytes, extract_content_from_bytes_with_query,
    extract_document_from_bytes, extract_document_from_bytes_with_query,
    extract_markdown_from_bytes, extract_markdown_from_bytes_with_query,
};
pub use error::{Error, Result};
pub use extract::{
    DetectMode, EdgePart, ExtractOptions, GlyphPart, GraphicPart, PagePart, ReaderSettings,
    extract_from_parts,
};
pub use model::{BBox, Cell, Document, Orientation, Page, Table};
pub use query::{ExtractQuery, PageSelection, Region, retain_parts_in_region};
#[cfg(feature = "reader")]
pub use reader::{
    PartsView, ReaderDump, ReaderDumpPage, TextView, extract_from_bytes,
    extract_from_bytes_with_query, extract_text_from_bytes, extract_text_from_bytes_with,
    extract_text_from_bytes_with_query, read_pages, read_pages_full, read_pages_full_with,
    read_pages_with,
};
#[cfg(feature = "text")]
pub use text::{
    TextChar, TextDoc, TextFont, TextItem, TextLine, TextPage, TextWord, build_text_items,
    build_text_items_with, build_text_lines, build_text_lines_with, retain_chars_in_region,
};
