# Building and feature flags

## Requirements

- Rust ≥ 1.85 (edition 2024)
- For wasm builds: `rustup target add wasm32-unknown-unknown`
- For browser packaging: [`wasm-pack`](https://rustwasm.github.io/wasm-pack/)

## Default build

```sh
cargo build --release
cargo test
```

The default features enable the full stack (`document`), including the CLI, the
document / Markdown pipeline, and all bundled CJK CMap groups.

## Feature flags

pdfni is organized into layered features. Higher layers pull in the layers
below them, so you only need to enable the top layer you want.

| Feature | Enables | Depends on |
| --- | --- | --- |
| `document` (default) | Document blocks + Markdown (`extract_document_from_bytes`, `document_to_markdown`) | `reader` |
| `reader` | PDF byte input (`extract_from_bytes`, `extract_text_from_bytes`, `read_pages`, CLI) | `text` |
| `text` | Coordinate-tagged text (`TextDoc`, items, lines) | — |
| _(none)_ | Table detection from parts JSON (`extract_from_parts`) — always available | — |
| `cmap-japan1` (default) | Predefined CJK CMap group Adobe-Japan1 | — |
| `cmap-gb1` (default) | Predefined CJK CMap group Adobe-GB1 | — |
| `cmap-cns1` (default) | Predefined CJK CMap group Adobe-CNS1 | — |
| `cmap-korea1` (default) | Predefined CJK CMap group Adobe-Korea1 | — |

The `cmap-*` features are independent of `reader` so you can pick only the
scripts you need. `Identity-H` / `Identity-V` (used by most modern CJK PDFs
that carry ToUnicode) work without any `cmap-*` feature.

**CMap trimming warning.** If you disable all `cmap-*` features, PDFs that use
predefined CJK CMap names (`90ms-RKSJ-H`, `UniJIS-UTF16-H`, `GB-EUC-H`,
`UniCNS-UTF16-H`, `KSCms-UHC-H`, ...) will return `Error::Reader("Unknown CMap
name: ...")`. Latin PDFs and PDFs using `Identity-H` continue to work.

## Common build shapes

```sh
# Full stack (same as default)
cargo build --release

# Document + Markdown, only Japan1
cargo build --release --no-default-features --features "document cmap-japan1"

# PDF byte input, coordinate text and tables, no document model
cargo build --release --no-default-features --features reader

# Coordinate text only (no PDF reader, no document model) — parts JSON only
cargo build --release --no-default-features --features text

# Table detection only (parts JSON in, tables out) — smallest build
cargo build --release --no-default-features
```

## wasm builds

```sh
# wasm with PDF byte input
cargo build --lib --target wasm32-unknown-unknown \
  --no-default-features --features reader

# wasm with the document / Markdown stack
cargo build --lib --target wasm32-unknown-unknown \
  --no-default-features --features document

# wasm, detection only (parts JSON in, tables out — smallest wasm)
cargo build --lib --target wasm32-unknown-unknown --no-default-features
```

## Browser package (wasm-pack)

See `test/wasm-test/README.md` for the smoke page. To produce a browser
package:

```sh
wasm-pack build --release --target web \
  --out-dir test/wasm-test/pkg \
  --no-default-features --features reader
node test/wasm-test/server.mjs   # http://localhost:8080
```
