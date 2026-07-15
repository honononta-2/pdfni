//! 純Rustリーダーの parts を pdfium_probe 同一スキーマで JSON 出力する

use pdfni::{GraphicPart, Orientation, read_pages};
use serde::Serialize;

#[derive(Serialize)]
struct Glyph {
    ch: char,
    x0: f64,
    x1: f64,
    top: f64,
    bottom: f64,
    /// デバイス座標のフォントサイズ（0 = 不明）
    fs: f64,
    upright: bool,
    /// 軸平行の回転角（0/90/180/270）
    rot: i32,
}

#[derive(Serialize)]
struct Edge {
    orient: &'static str,
    x0: f64,
    x1: f64,
    top: f64,
    bottom: f64,
}

#[derive(Serialize)]
struct Graphic {
    x0: f64,
    x1: f64,
    top: f64,
    bottom: f64,
    /// Total chord length of non-axis-parallel segments
    curve_len: f64,
}

#[derive(Serialize)]
struct PageOut {
    width: f64,
    height: f64,
    rotate: i32,
    /// リーダーが正規化したページ内容の支配回転（0/90/180/270）
    norm_rotate: i32,
    glyphs: Vec<Glyph>,
    edges: Vec<Edge>,
    /// 非罫線グラフィックの外接矩形
    graphics: Vec<Graphic>,
}

#[derive(Serialize)]
struct DocOut {
    source: String,
    pages: Vec<PageOut>,
}

fn main() {
    let mut args = std::env::args().skip(1);
    let input = args.next().unwrap_or_else(|| {
        eprintln!("usage: reader_probe <input.pdf> <output.json>");
        std::process::exit(2);
    });
    let out_path = args.next().unwrap_or_else(|| {
        eprintln!("usage: reader_probe <input.pdf> <output.json>");
        std::process::exit(2);
    });

    let bytes = match std::fs::read(&input) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("failed to read {input}: {e}");
            std::process::exit(1);
        }
    };

    let pages_in = match read_pages(&bytes, None) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("read failed: {e}");
            std::process::exit(1);
        }
    };

    let pages: Vec<PageOut> = pages_in
        .into_iter()
        .map(|p| PageOut {
            width: p.width,
            height: p.height,
            rotate: 0,
            norm_rotate: p.norm_rotate,
            glyphs: p
                .glyphs
                .into_iter()
                .map(|g| Glyph {
                    ch: g.ch,
                    x0: g.left,
                    x1: g.right,
                    top: g.top,
                    bottom: g.bottom,
                    fs: g.font_size.unwrap_or(0.0),
                    upright: g.upright,
                    rot: g.rot,
                })
                .collect(),
            graphics: p
                .graphics
                .iter()
                .map(|g: &GraphicPart| Graphic {
                    x0: g.left,
                    x1: g.right,
                    top: g.top,
                    bottom: g.bottom,
                    curve_len: g.curve_len,
                })
                .collect(),
            edges: p
                .edges
                .into_iter()
                .map(|e| Edge {
                    orient: match e.orientation {
                        Orientation::Vertical => "V",
                        Orientation::Horizontal => "H",
                    },
                    x0: e.left,
                    x1: e.right,
                    top: e.top,
                    bottom: e.bottom,
                })
                .collect(),
        })
        .collect();

    let out = DocOut {
        source: input,
        pages,
    };
    let json = serde_json::to_string(&out).unwrap();
    if let Err(e) = std::fs::write(&out_path, json) {
        eprintln!("failed to write {out_path}: {e}");
        std::process::exit(1);
    }
    eprintln!("=> {out_path}");
    for pg in &out.pages {
        eprintln!(
            "  page rotate={} {:.0}x{:.0} glyphs={} edges={}",
            pg.rotate,
            pg.width,
            pg.height,
            pg.glyphs.len(),
            pg.edges.len()
        );
    }
}
