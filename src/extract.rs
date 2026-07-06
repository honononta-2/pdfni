//! 抽出のオーケストレーション

use crate::detect_hybrid::{self, HybridSettings};
use crate::detect_lattice::{self, LatticeSettings};
use crate::detect_text::{self, Seg, TextSettings};
use crate::model::{BBox, Cell, Document, Edge, Glyph, Orientation, Page, Table};
use serde::Deserialize;

#[cfg(feature = "pdfium")]
use crate::error::Result;
#[cfg(feature = "pdfium")]
use crate::reader;
#[cfg(feature = "pdfium")]
use pdfium_render::prelude::*;
#[cfg(all(feature = "pdfium", not(target_arch = "wasm32")))]
use std::path::Path;
#[cfg(all(feature = "pdfium", not(target_arch = "wasm32")))]
use std::sync::OnceLock;

/// 外部リーダーから受け取る1ページ分の文字と線分
#[derive(Debug, Clone, Deserialize)]
pub struct PagePart {
    pub width: f64,
    pub height: f64,
    pub glyphs: Vec<GlyphPart>,
    pub edges: Vec<EdgePart>,
}

/// 外部リーダーから受け取る1文字
#[derive(Debug, Clone, Deserialize)]
pub struct GlyphPart {
    pub ch: char,
    pub left: f64,
    pub right: f64,
    pub top: f64,
    pub bottom: f64,
}

/// 外部リーダーから受け取る軸平行な線分
#[derive(Debug, Clone, Deserialize)]
pub struct EdgePart {
    pub orientation: Orientation,
    pub left: f64,
    pub right: f64,
    pub top: f64,
    pub bottom: f64,
}

/// 実行する検出方式の選択
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DetectMode {
    /// 3方式すべて
    #[default]
    Auto,
    /// 罫線を使う方式のみ
    Ruled,
    /// 文字整列方式のみ
    Borderless,
}

/// 抽出処理のパラメータ
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct ExtractOptions {
    pub mode: DetectMode,
    pub lattice: LatticeSettings,
    pub hybrid: HybridSettings,
    pub text: TextSettings,
}

#[cfg(all(feature = "pdfium", not(target_arch = "wasm32")))]
static PDFIUM: OnceLock<Pdfium> = OnceLock::new();

#[cfg(all(feature = "pdfium", not(target_arch = "wasm32")))]
pub fn set_pdfium_dll_dir(dir: impl AsRef<Path>) -> Result<()> {
    if PDFIUM.get().is_some() {
        return Err(crate::error::Error::PdfiumBind(
            "pdfium is already initialized; set_pdfium_dll_dir must be called before any extract()"
                .into(),
        ));
    }
    let pdfium = reader::bind_pdfium(Some(dir.as_ref()))?;
    let _ = PDFIUM.set(pdfium);
    Ok(())
}

#[cfg(all(feature = "pdfium", not(target_arch = "wasm32")))]
fn pdfium() -> Result<&'static Pdfium> {
    if let Some(p) = PDFIUM.get() {
        return Ok(p);
    }
    let bound = reader::bind_pdfium(None)?;
    let _ = PDFIUM.set(bound);
    Ok(PDFIUM.get().expect("just initialized"))
}

fn is_cjk(c: char) -> bool {
    matches!(c as u32,
        0x3000..=0x303F   // CJK記号・句読点
        | 0x3040..=0x30FF // ひらがな・カタカナ
        | 0x3400..=0x4DBF // 拡張漢字A
        | 0x4E00..=0x9FFF // 漢字
        | 0xFF61..=0xFF9F // 半角カナ
    )
}

/// 行内の水平な空白か
fn is_inline_space(c: char) -> bool {
    c == ' ' || c == '\u{3000}' || c == '\t'
}

/// CJK文字どうしの間の空白を除く
fn strip_cjk_spaces(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::new();
    for i in 0..chars.len() {
        if is_inline_space(chars[i]) {
            let prev = out.chars().last();
            let next = chars[i + 1..].iter().find(|&&x| !is_inline_space(x)).copied();
            if let (Some(p), Some(n)) = (prev, next) {
                if is_cjk(p) && is_cjk(n) {
                    continue;
                }
            }
        }
        out.push(chars[i]);
    }
    out
}

/// セル内テキストの組み立て
fn cell_text(chars: &[Glyph], cell: &BBox) -> String {
    let mut hits: Vec<(usize, &Glyph)> = chars
        .iter()
        .enumerate()
        .filter(|(_, g)| cell.contains_center(&g.bbox))
        .collect();
    if hits.is_empty() {
        return String::new();
    }

    let mut heights: Vec<f64> = hits.iter().map(|(_, g)| g.bbox.height()).collect();
    heights.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let median_h = heights[heights.len() / 2];
    let line_tol = (median_h * 0.6).max(0.5);

    hits.sort_by(|a, b| a.1.bbox.bottom.partial_cmp(&b.1.bbox.bottom).unwrap());
    let mut lines: Vec<Vec<(usize, &Glyph)>> = Vec::new();
    let mut cur: Vec<(usize, &Glyph)> = Vec::new();
    let mut last_bottom = f64::NEG_INFINITY;
    for (i, g) in hits {
        if !cur.is_empty() && g.bbox.bottom - last_bottom > line_tol {
            lines.push(std::mem::take(&mut cur));
        }
        last_bottom = g.bbox.bottom;
        cur.push((i, g));
    }
    if !cur.is_empty() {
        lines.push(cur);
    }

    // 縦積み文字の原文順での連結
    let solid = |l: &[(usize, &Glyph)]| l.iter().filter(|(_, g)| !g.ch.is_whitespace()).count();
    if lines.len() >= 2
        && lines.iter().all(|l| solid(l) <= 1)
        && lines.iter().filter(|l| solid(l) == 1).count() >= 2
    {
        let mut idxs: Vec<usize> = lines.iter().flatten().map(|&(i, _)| i).collect();
        idxs.sort_unstable();
        let s: String = idxs.iter().map(|&i| chars[i].ch).collect();
        return strip_cjk_spaces(s.trim());
    }

    let mut out = Vec::with_capacity(lines.len());
    for mut line in lines {
        line.sort_by(|a, b| a.1.bbox.x0.partial_cmp(&b.1.bbox.x0).unwrap());
        let s: String = line.iter().map(|(_, g)| g.ch).collect();
        out.push(s.trim().to_string());
    }
    strip_cjk_spaces(&out.join("\n"))
}

/// 文字を行内の塊に切り分ける
fn build_segments(chars: &[Glyph]) -> Vec<Seg> {
    if chars.is_empty() {
        return Vec::new();
    }
    let mut heights: Vec<f64> = chars.iter().map(|g| g.bbox.height()).collect();
    heights.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let med_h = heights[heights.len() / 2];
    let line_tol = (med_h * 0.6).max(0.5);
    let seg_gap = (med_h).max(1.0);

    let mut order: Vec<usize> = (0..chars.len()).collect();
    order.sort_by(|&a, &b| {
        chars[a]
            .bbox
            .bottom
            .partial_cmp(&chars[b].bbox.bottom)
            .unwrap()
    });

    let mut lines: Vec<Vec<usize>> = Vec::new();
    let mut cur: Vec<usize> = Vec::new();
    let mut last_b = f64::NEG_INFINITY;
    for &i in &order {
        if !cur.is_empty() && chars[i].bbox.bottom - last_b > line_tol {
            lines.push(std::mem::take(&mut cur));
        }
        last_b = chars[i].bbox.bottom;
        cur.push(i);
    }
    if !cur.is_empty() {
        lines.push(cur);
    }

    let mut segs = Vec::new();
    for mut line in lines {
        line.sort_by(|&a, &b| chars[a].bbox.x0.partial_cmp(&chars[b].bbox.x0).unwrap());
        let mut text = String::new();
        let mut bb: Option<BBox> = None;
        let mut last_x1 = f64::NEG_INFINITY;
        for &i in &line {
            let g = &chars[i];
            if bb.is_some() && g.bbox.x0 - last_x1 > seg_gap {
                push_seg(&mut segs, &mut text, &mut bb);
            }
            if !g.ch.is_whitespace() || bb.is_some() {
                text.push(g.ch);
            }
            // 空白を除いた bbox の拡張
            if !g.ch.is_whitespace() {
                bb = Some(match bb {
                    Some(b) => BBox {
                        x0: b.x0.min(g.bbox.x0),
                        top: b.top.min(g.bbox.top),
                        x1: b.x1.max(g.bbox.x1),
                        bottom: b.bottom.max(g.bbox.bottom),
                    },
                    None => g.bbox,
                });
                last_x1 = g.bbox.x1;
            }
        }
        push_seg(&mut segs, &mut text, &mut bb);
    }
    segs
}

fn push_seg(segs: &mut Vec<Seg>, text: &mut String, bb: &mut Option<BBox>) {
    if let Some(b) = bb.take() {
        let trimmed = strip_cjk_spaces(text.trim());
        if !trimmed.is_empty() {
            segs.push(Seg {
                text: trimmed,
                bbox: b,
            });
        }
    }
    text.clear();
}

/// 矩形の重なり判定
fn overlaps_any(b: &BBox, tables: &[Table]) -> bool {
    tables.iter().any(|t| {
        let ox0 = b.x0.max(t.bbox.x0);
        let ox1 = b.x1.min(t.bbox.x1);
        let oy0 = b.top.max(t.bbox.top);
        let oy1 = b.bottom.min(t.bbox.bottom);
        let ow = (ox1 - ox0).max(0.0);
        let oh = (oy1 - oy0).max(0.0);
        let oarea = ow * oh;
        let barea = b.width() * b.height();
        barea > 0.0 && oarea / barea > 0.3
    })
}

/// 全行が空の列と全列が空の行を落とす
fn prune_empty(t: &mut Table) {
    let keep_cols: Vec<usize> = (0..t.n_cols)
        .filter(|&c| {
            t.data
                .iter()
                .any(|r| c < r.len() && !r[c].text.trim().is_empty())
        })
        .collect();
    let keep_rows: Vec<usize> = (0..t.data.len())
        .filter(|&r| t.data[r].iter().any(|cell| !cell.text.trim().is_empty()))
        .collect();

    let data: Vec<Vec<Cell>> = keep_rows
        .iter()
        .map(|&r| keep_cols.iter().map(|&c| t.data[r][c].clone()).collect())
        .collect();
    t.n_rows = data.len();
    t.n_cols = keep_cols.len();
    t.data = data;
}

/// PDFからの全ページ表抽出
#[cfg(all(feature = "pdfium", not(target_arch = "wasm32")))]
pub fn extract(path: impl AsRef<Path>, password: Option<&str>) -> Result<Document> {
    extract_with_options(path, password, &ExtractOptions::default())
}

#[cfg(all(feature = "pdfium", not(target_arch = "wasm32")))]
pub fn extract_with_options(
    path: impl AsRef<Path>,
    password: Option<&str>,
    options: &ExtractOptions,
) -> Result<Document> {
    let pdfium = pdfium()?;
    let path = path.as_ref();
    let doc = pdfium.load_pdf_from_file(path, password)?;
    let source = path.to_string_lossy().into_owned();
    Ok(Document {
        source,
        pages: extract_pages(&doc, options)?,
    })
}

/// バイト列からの全ページ表抽出
#[cfg(all(feature = "pdfium", not(target_arch = "wasm32")))]
pub fn extract_from_bytes(bytes: &[u8], password: Option<&str>) -> Result<Document> {
    extract_from_bytes_with_options(bytes, password, &ExtractOptions::default())
}

#[cfg(all(feature = "pdfium", not(target_arch = "wasm32")))]
pub fn extract_from_bytes_with_options(
    bytes: &[u8],
    password: Option<&str>,
    options: &ExtractOptions,
) -> Result<Document> {
    let pdfium = pdfium()?;
    let doc = pdfium.load_pdf_from_byte_vec(bytes.to_vec(), password)?;
    Ok(Document {
        source: "<bytes>".to_string(),
        pages: extract_pages(&doc, options)?,
    })
}

/// pdfium で開いた文書からの全ページ表抽出
#[cfg(feature = "pdfium")]
fn extract_pages(doc: &PdfDocument<'_>, options: &ExtractOptions) -> Result<Vec<Page>> {
    let mut pages = Vec::new();
    for (pi, page) in doc.pages().iter().enumerate() {
        let h = page.height().value as f64;
        let w = page.width().value as f64;
        let edges = reader::extract_edges(&page, h);
        let chars = reader::extract_chars(&page, h)?;
        let tables = detect_tables(&chars, &edges, options);
        pages.push(Page {
            page_number: pi + 1,
            width: w,
            height: h,
            tables,
        });
    }
    Ok(pages)
}

/// wasm 上の pdfium でバイト列から全ページ表抽出
#[cfg(all(feature = "pdfium", target_arch = "wasm32"))]
pub fn extract_from_bytes_wasm(bytes: &[u8], options: &ExtractOptions) -> Result<Document> {
    let pdfium = reader::bind_pdfium()?;
    let doc = pdfium.load_pdf_from_byte_vec(bytes.to_vec(), None)?;
    Ok(Document {
        source: "<pdf>".to_string(),
        pages: extract_pages(&doc, options)?,
    })
}

/// 外部リーダーが取り出した文字と線分からの全ページ表抽出
pub fn extract_from_parts(pages: Vec<PagePart>, options: &ExtractOptions) -> Document {
    let out = pages
        .into_iter()
        .enumerate()
        .map(|(pi, part)| {
            let chars: Vec<Glyph> = part
                .glyphs
                .into_iter()
                .map(|g| Glyph {
                    ch: g.ch,
                    bbox: BBox {
                        x0: g.left,
                        top: g.top,
                        x1: g.right,
                        bottom: g.bottom,
                    },
                })
                .collect();
            let edges: Vec<Edge> = part
                .edges
                .into_iter()
                .map(|e| Edge {
                    x0: e.left,
                    top: e.top,
                    x1: e.right,
                    bottom: e.bottom,
                    orientation: e.orientation,
                })
                .collect();
            Page {
                page_number: pi + 1,
                width: part.width,
                height: part.height,
                tables: detect_tables(&chars, &edges, options),
            }
        })
        .collect();
    Document {
        source: "<parts>".to_string(),
        pages: out,
    }
}

/// 3方式の検出を優先順で実行し1ページ分の表を確定する
fn detect_tables(chars: &[Glyph], edges: &[Edge], options: &ExtractOptions) -> Vec<Table> {
    let fill = |b: &BBox| cell_text(chars, b);
    let use_ruled = options.mode != DetectMode::Borderless;
    let use_text = options.mode != DetectMode::Ruled;
    let mut tables = if use_ruled {
        detect_lattice::detect(edges, &fill, &options.lattice)
    } else {
        Vec::new()
    };
    for t in &mut tables {
        prune_empty(t);
    }
    tables.retain(|t| t.n_rows >= 2 && t.n_cols >= 2);

    // 確定したlattice表の領域内の文字は後続の検出の入力から除く
    let segs: Vec<Seg> = build_segments(chars)
        .into_iter()
        .filter(|seg| !tables.iter().any(|t| t.bbox.contains_center(&seg.bbox)))
        .collect();

    let mut hybrid_tables = if use_ruled {
        detect_hybrid::detect(&segs, edges, &fill, &options.hybrid)
    } else {
        Vec::new()
    };
    for t in &mut hybrid_tables {
        prune_empty(t);
    }
    for t in hybrid_tables {
        if t.n_rows >= 2 && t.n_cols >= 2 && !overlaps_any(&t.bbox, &tables) {
            tables.push(t);
        }
    }

    // 確定したhybrid表の領域内の文字も除いてからtext検出に渡す
    let segs: Vec<Seg> = segs
        .into_iter()
        .filter(|seg| !tables.iter().any(|t| t.bbox.contains_center(&seg.bbox)))
        .collect();
    let mut text_tables = if use_text {
        detect_text::detect(&segs, &options.text)
    } else {
        Vec::new()
    };
    for t in &mut text_tables {
        prune_empty(t);
    }
    for t in text_tables {
        if t.n_rows >= 2 && t.n_cols >= 2 && !overlaps_any(&t.bbox, &tables) {
            tables.push(t);
        }
    }
    tables
}
