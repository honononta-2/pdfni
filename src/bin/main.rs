//! PDFから文書内容・表を抽出するCLI

use anyhow::Result;
use pdfni::{DetectMode, ExtractOptions, ExtractQuery, Region};
use std::env;
use std::io::{BufRead, IsTerminal};

fn usage() -> ! {
    eprintln!(
        "usage: pdfni [--output content|tables|doc] [--mode auto|ruled|borderless] [--pages 1,3-5] [--region L,T,R,B] [--detect-header-footer] [--password-stdin] <input.pdf> [output.json]"
    );
    std::process::exit(2);
}

enum OutputKind {
    Content,
    Tables,
    Doc,
}

// 矩形は top-down 表示座標
fn parse_region(s: &str) -> Option<Region> {
    let vals: Vec<f64> = s
        .split(',')
        .map(|t| t.trim().parse::<f64>())
        .collect::<std::result::Result<_, _>>()
        .ok()?;
    let &[left, top, right, bottom] = vals.as_slice() else {
        return None;
    };
    Some(Region::from_ltrb(left, top, right, bottom))
}

fn read_password_from_stdin() -> Result<String> {
    let stdin = std::io::stdin();
    if stdin.is_terminal() {
        eprintln!("--password-stdin was given but stdin is a TTY; pipe or redirect the password");
        std::process::exit(2);
    }
    let mut line = String::new();
    stdin.lock().read_line(&mut line)?;
    if line.ends_with('\n') {
        line.pop();
        if line.ends_with('\r') {
            line.pop();
        }
    }
    Ok(line)
}

fn main() -> Result<()> {
    let mut output_kind = OutputKind::Content;
    let mut mode = DetectMode::Auto;
    let mut detect_header_footer = false;
    let mut password_stdin = false;
    let mut query = ExtractQuery::default();
    let mut positional = Vec::new();
    let mut args = env::args().skip(1);
    while let Some(a) = args.next() {
        if a == "--output" {
            output_kind = match args.next().as_deref() {
                Some("content") => OutputKind::Content,
                Some("tables") => OutputKind::Tables,
                Some("doc") => OutputKind::Doc,
                _ => usage(),
            };
        } else if a == "--mode" {
            mode = match args.next().as_deref() {
                Some("auto") => DetectMode::Auto,
                Some("ruled") => DetectMode::Ruled,
                Some("borderless") => DetectMode::Borderless,
                _ => usage(),
            };
        } else if a == "--pages" {
            query.pages = match args.next().map(|s| s.parse()) {
                Some(Ok(sel)) => Some(sel),
                _ => usage(),
            };
        } else if a == "--region" {
            query.region = match args.next().as_deref().map(parse_region) {
                Some(Some(region)) => Some(region),
                _ => usage(),
            };
        } else if a == "--detect-header-footer" {
            detect_header_footer = true;
        } else if a == "--password-stdin" {
            password_stdin = true;
        } else {
            positional.push(a);
        }
    }
    let mut positional = positional.into_iter();
    let input = positional.next().unwrap_or_else(|| usage());
    let output = positional.next();
    if positional.next().is_some() {
        usage();
    }

    let password = if password_stdin {
        Some(read_password_from_stdin()?)
    } else {
        None
    };

    let options = ExtractOptions {
        mode,
        detect_header_footer,
        ..ExtractOptions::default()
    };
    let bytes = std::fs::read(&input)?;
    let out_text = match output_kind {
        OutputKind::Content => {
            let doc = pdfni::extract_content_from_bytes_with_query(
                &bytes,
                password.as_deref(),
                &options,
                &query,
            )?;
            serde_json::to_string_pretty(&doc)?
        }
        OutputKind::Tables => {
            let mut doc = pdfni::extract_from_bytes_with_query(
                &bytes,
                password.as_deref(),
                &options,
                &query,
            )?;
            doc.source = input.clone();
            serde_json::to_string_pretty(&doc)?
        }
        OutputKind::Doc => {
            let doc = pdfni::extract_document_from_bytes_with_query(
                &bytes,
                password.as_deref(),
                &options,
                &query,
            )?;
            serde_json::to_string_pretty(&doc)?
        }
    };

    match output {
        Some(out) => {
            std::fs::write(&out, out_text)?;
            eprintln!("=> {out}");
        }
        None => println!("{out_text}"),
    }
    Ok(())
}
