# reticella

A Rust library that extracts tables and Markdown from text-embedded PDFs.
To pursue both accuracy and speed, it ships with its own pure-Rust PDF
reader adapted from [Mozilla pdf.js](https://github.com/mozilla/pdf.js).
Usable as a Rust crate, a CLI, or a WebAssembly module from Node.js and
the browser.

> Early software (`0.1.0`). APIs may still change.

## Highlights

- Table extraction — solid for ruled and text-aligned layouts; support for merged cells is limited ([how it works](docs/ARCHITECTURE.md#table-detectors))
- Markdown extraction from the document model (paragraphs, headings, lists, tables) ([pipeline](docs/ARCHITECTURE.md#pipeline))
- Built-in pure-Rust PDF reader adapted from pdf.js — no OCR, no page rasterization, no system PDF library ([reader lineage](docs/ARCHITECTURE.md#reader-lineage))
- Native, Node.js, and browser via WebAssembly ([wasm guide](docs/WASM.md))

## What it is not

- OCR for scanned / image-only PDFs
- A full PDF renderer (images, annotations, forms, signatures are out of scope)
- Vertical-writing-aware **table** detection (the reader can surface vertical text; table detection stays oriented to horizontal layout)

## Quick Start

### CLI

```sh
cargo run --release -- input.pdf output.json
```

Prints the table `Document` JSON to `output.json`. See [docs/CLI.md](docs/CLI.md)
for modes and encrypted PDFs.

### Rust

```rust
use reticella::{extract_from_bytes, ExtractOptions};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let bytes = std::fs::read("input.pdf")?;
    let doc = extract_from_bytes(&bytes, None, &ExtractOptions::default())?;
    println!("{}", serde_json::to_string_pretty(&doc)?);
    Ok(())
}
```

For document blocks and Markdown, use `extract_document_from_bytes` and
`document_to_markdown` (requires the `document` feature — enabled by default).

## Documentation

- [Building and feature flags](docs/BUILD.md)
- [CLI usage](docs/CLI.md)
- [WebAssembly (Node.js and browser)](docs/WASM.md)
- [Architecture and internals](docs/ARCHITECTURE.md)
- [API reference on docs.rs](https://docs.rs/reticella)

## License

Licensed under the Apache License, Version 2.0. See [LICENSE](LICENSE).

## Third-party notices

The bundled PDF reader is adapted from [Mozilla pdf.js](https://github.com/mozilla/pdf.js),
and the bundled binary CMap data is derived from
[Adobe CMap resources](https://github.com/adobe-type-tools/cmap-resources).
See [NOTICE](NOTICE) for attribution and license details.
