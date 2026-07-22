# WebAssembly (Node.js and browser)

pdfni exposes the same detection and extraction paths through
`wasm-bindgen`. The exports available depend on which features are enabled at
build time.

## Building

```sh
# For Node.js (with all bundled CJK CMap groups)
wasm-pack build --release --target nodejs \
  --no-default-features \
  --features "document cmap-japan1 cmap-gb1 cmap-cns1 cmap-korea1"

# For the browser (with all bundled CJK CMap groups)
wasm-pack build --release --target web \
  --no-default-features \
  --features "document cmap-japan1 cmap-gb1 cmap-cns1 cmap-korea1"

# Smallest wasm — detection only (parts JSON in, tables out)
wasm-pack build --release --target web --no-default-features
```

Swap `document` for `reader` if you don't need Markdown / document blocks,
or drop it entirely to build a detector-only bundle that accepts parts JSON.
Trim the `cmap-*` list to only the scripts you need to shrink the wasm — but
note that dropping every `cmap-*` feature causes PDFs using predefined CJK
CMap names (`90ms-RKSJ-H`, `UniJIS-UTF16-H`, `GB-EUC-H`, `UniCNS-UTF16-H`,
`KSCms-UHC-H`, ...) to fail with `Error::Reader`. See [BUILD.md](BUILD.md)
for the full feature layering and the CMap trimming warning.

## JS exports

| Export | Required feature | Purpose |
| --- | --- | --- |
| `extract_tables(input_json)` | — (always available) | parts JSON → table `Document` JSON |
| `extract_tables_from_pdf(bytes, options_json?, password?)` | `reader` | PDF bytes → table `Document` JSON |
| `extract_text_from_pdf(bytes, password?, options_json?)` | `reader` | PDF bytes → `TextDoc` JSON |
| `extract_text_items_from_pdf(bytes, password?, options_json?)` | `reader` | PDF bytes → pdf.js-like items per page |
| `extract_text_lines_from_pdf(bytes, password?, options_json?)` | `reader` | PDF bytes → line / word view per page |
| `extract_document_from_pdf(bytes, options_json?, password?)` | `document` | PDF bytes → `DocDoc` JSON (blocks + tables) |
| `extract_content_from_pdf(bytes, options_json?, password?)` | `document` | PDF bytes → `DocContent` JSON (blocks + tables, no per-glyph data) |
| `extract_markdown_from_pdf(bytes, options_json?, password?)` | `document` | PDF bytes → Markdown string |

`options_json` accepts the same JSON that `ExtractOptions` deserializes on the
Rust side (fields like `mode`, `detect_lists`, `escape_markdown`, `bidi`,
`detect_header_footer`, and the `reader` section — see
[ARCHITECTURE.md](ARCHITECTURE.md#resource-limits) for
`reader.max_decoded_bytes`). The text helpers (`extract_text_from_pdf`,
`extract_text_items_from_pdf`, `extract_text_lines_from_pdf`) take
`options_json` as their last argument and only read the `reader` section.
Bidi is off in the `extract_text_items_from_pdf`
and `extract_text_lines_from_pdf` helpers — enable it via `options_json` on
the document / Markdown paths, or use the Rust `_with` APIs directly.

## Browser smoke page

An end-to-end browser example lives under `demo/`. Build the wasm package
with `wasm-pack build --release --target web --out-dir demo/pkg` (default
features — the page imports document-level APIs), then serve it with
`node demo/server.mjs`.
