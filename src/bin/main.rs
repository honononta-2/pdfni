//! PDFから表を抽出しJSONを出すCLI
//! 使い方: pdfni [--mode auto|ruled|borderless] [--pages 1,3-5] [--region L,T,R,B] [--password-stdin] <input.pdf> [output.json]

use anyhow::Result;
use pdfni::{DetectMode, ExtractOptions, ExtractQuery, Region};
use std::env;
use std::io::{BufRead, IsTerminal};

fn usage() -> ! {
    eprintln!(
        "使い方: pdfni [--mode auto|ruled|borderless] [--pages 1,3-5] [--region L,T,R,B] [--password-stdin] <input.pdf> [output.json]"
    );
    std::process::exit(2);
}

// L,T,R,B の4値を top-down 表示座標の矩形として読む
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

// stdin の 1 行目をパスワードとして読む。末尾の \n / \r\n は 1 回だけ除去
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
    let mut mode = DetectMode::Auto;
    let mut password_stdin = false;
    let mut query = ExtractQuery::default();
    let mut positional = Vec::new();
    let mut args = env::args().skip(1);
    while let Some(a) = args.next() {
        if a == "--mode" {
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
        ..ExtractOptions::default()
    };
    let bytes = std::fs::read(&input)?;
    let mut doc = pdfni::extract_from_bytes_with_query(&bytes, password.as_deref(), &options, &query)?;
    doc.source = input.clone();
    let json = serde_json::to_string_pretty(&doc)?;

    match output {
        Some(out) => {
            std::fs::write(&out, json)?;
            eprintln!("=> {out}");
        }
        None => println!("{json}"),
    }
    Ok(())
}
