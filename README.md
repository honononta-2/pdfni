# pdfni

A Rust library that extracts tables and Markdown from text-embedded PDFs.
To pursue both accuracy and speed, it ships with its own pure-Rust PDF
reader adapted from [Mozilla pdf.js](https://github.com/mozilla/pdf.js).
Usable as a Rust crate, a CLI, or a WebAssembly module from Node.js and
the browser.

> Expect breaking changes.

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

Prints the content JSON (text blocks and tables in reading order) to
`output.json`. Use `--output tables` for the table-only `Document` JSON, or
`--output doc` for the full document model. See [docs/CLI.md](docs/CLI.md)
for options, modes, and encrypted PDFs.

### Rust

```rust
use pdfni::{extract_from_bytes, ExtractOptions};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let bytes = std::fs::read("input.pdf")?;
    let doc = extract_from_bytes(&bytes, None, &ExtractOptions::default())?;
    println!("{}", serde_json::to_string_pretty(&doc)?);
    Ok(())
}
```

## Benchmarks

Full pipeline (table regions are not given), scored with each benchmark's
official evaluation tool. Scripts and test data live under
[`bench/`](bench/) — see each benchmark's README for setup.

| Benchmark | Task | Precision | Recall | F1 |
| --- | --- | --- | --- | --- |
| [ICDAR 2013 original](http://www.tamirhassan.com/html/dataset.html) (eu + us) | Region detection (char-level) | 0.9382 | 0.9316 | 0.9349 |
| [ICDAR 2013 original](http://www.tamirhassan.com/html/dataset.html) (eu + us) | Structure (cell adjacency) | 0.9522 | 0.9414 | 0.9468 |
| [ICDAR 2013 corrected](https://huggingface.co/datasets/bsmock/ICDAR-2013-Table-Competition-Corrected) (eu + us) | Region detection (char-level) | 0.9382 | 0.9316 | 0.9349 |
| [ICDAR 2013 corrected](https://huggingface.co/datasets/bsmock/ICDAR-2013-Table-Competition-Corrected) (eu + us) | Structure (cell adjacency) | 0.9522 | 0.9414 | 0.9468 |
| [SciTSR](https://github.com/Academic-Hammer/SciTSR) test (3,000 tables) | Structure (cell adjacency, macro) | 0.9254 | 0.8997 | 0.9124 |

## Documentation

- [Building and feature flags](docs/BUILD.md)
- [CLI usage](docs/CLI.md)
- [WebAssembly (Node.js and browser)](docs/WASM.md)
- [C ABI (native)](docs/FFI.md)
- [Architecture and internals](docs/ARCHITECTURE.md)
- [API reference on docs.rs](https://docs.rs/pdfni)

## License

Licensed under the Apache License, Version 2.0. See [LICENSE](LICENSE).

## Third-party notices

The bundled PDF reader is adapted from [Mozilla pdf.js](https://github.com/mozilla/pdf.js),
and the bundled binary CMap data is derived from
[Adobe CMap resources](https://github.com/adobe-type-tools/cmap-resources).
See [NOTICE](NOTICE) for attribution and license details.
