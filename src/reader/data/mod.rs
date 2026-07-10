//! 同梱データ（pdf.js 由来の静的表と .bcmap）

// フォント表は tools/gen_font_data.py が生成
pub mod encodings;
pub mod glyphlist;
pub mod metrics;
pub mod std_fonts;

// .bcmap 索引は tools/gen_bcmap_index.py が生成
pub mod bcmaps;

pub use encodings::*;
pub use glyphlist::*;
pub use metrics::*;
pub use std_fonts::*;
