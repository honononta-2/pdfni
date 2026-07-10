//! 座標付き文字を JSON で出力する

use reticella::{build_text_items, extract_text_from_bytes};

fn main() {
    let mut args = std::env::args().skip(1);
    let input = args.next().unwrap_or_else(|| {
        eprintln!("usage: text_probe <input.pdf> [items] [output.json] [password]");
        std::process::exit(2);
    });

    let mut items_mode = false;
    let mut out_path = None;
    let mut password = None;

    if let Some(a) = args.next() {
        if a == "items" {
            items_mode = true;
            out_path = args.next();
            password = args.next();
        } else {
            out_path = Some(a);
            password = args.next();
        }
    }

    let bytes = match std::fs::read(&input) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("failed to read {input}: {e}");
            std::process::exit(1);
        }
    };

    let doc = match extract_text_from_bytes(&bytes, password.as_deref()) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("extract failed: {e}");
            std::process::exit(1);
        }
    };

    let json = if items_mode {
        let pages: Vec<_> = doc.pages.iter().map(build_text_items).collect();
        match serde_json::to_string_pretty(&pages) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("serialize failed: {e}");
                std::process::exit(1);
            }
        }
    } else {
        match serde_json::to_string_pretty(&doc) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("serialize failed: {e}");
                std::process::exit(1);
            }
        }
    };

    if let Some(path) = out_path {
        if let Err(e) = std::fs::write(&path, &json) {
            eprintln!("failed to write {path}: {e}");
            std::process::exit(1);
        }
        eprintln!("=> {path}");
    } else {
        println!("{json}");
    }

    if items_mode {
        for (i, pg) in doc.pages.iter().enumerate() {
            let n = build_text_items(pg).len();
            eprintln!(
                "  page {i} {:.0}x{:.0} fonts={} items={}",
                pg.width,
                pg.height,
                pg.fonts.len(),
                n
            );
        }
    } else {
        for (i, pg) in doc.pages.iter().enumerate() {
            eprintln!(
                "  page {i} {:.0}x{:.0} fonts={} chars={}",
                pg.width,
                pg.height,
                pg.fonts.len(),
                pg.chars.len()
            );
        }
    }
}
