# Architecture

## Pipeline

```
PDF bytes
  → pure-Rust reader (glyphs, edges, graphics; text chars)
      ├→ read_pages_full        → ReaderDump (parts + text) JSON
      ├→ extract_from_bytes     → Document (tables) JSON
      ├→ extract_document_from_bytes → DocDoc (blocks + tables) JSON
      └→ extract_markdown_from_bytes → Markdown string

  ↘ TextDoc → items / lines
```

Each byte-input entry point covers one output level and is a one-shot API:
`read_pages_full` returns the reader's full dump (parts + text) in one pass,
`extract_from_bytes` runs table detection, `extract_document_from_bytes` adds
block-level structure (headings / lists / header / footer), and
`extract_markdown_from_bytes` finishes with Markdown. The document and Markdown
entries share the single content-stream pass through the reader so it is not
run twice.

## Coordinates

Coordinates are top-down, in PDF points (1/72 inch). The origin is the top-left
of the page's view rectangle — the intersection of `CropBox` and `MediaBox`,
falling back to `MediaBox` when the intersection is empty or invalid — after
the page `/Rotate` is applied. Pages that carry a single dominant rotation
(90 / 180 / 270 degrees) are normalized inside the reader; the detected table
bounding boxes are returned in the original page coordinate frame.

## Table detectors

Three detectors run in sequence under `DetectMode::Auto`. Regions already
covered by a confirmed table are removed before the next detector runs, so
they do not overlap.

| Detector | What it looks at | Best for |
| --- | --- | --- |
| `lattice` | Axis-aligned ruled edges | Tables with visible ruling |
| `hybrid` | Ruled edges + text alignment | Partially ruled tables |
| `text` | Text alignment only | Borderless tables |

`DetectMode::Ruled` runs `lattice` only. `DetectMode::Borderless` runs `text`
only.

## Reader lineage

The PDF reader under `src/reader/` is ported in part from
[Mozilla pdf.js](https://github.com/mozilla/pdf.js) and substantially modified
for this library. Bundled encodings, glyph list, standard font metrics, and
binary CMaps under `src/reader/data/` are generated from **pdf.js 6.0.227**.
CMap binaries are derived from
[Adobe CMap resources](https://github.com/adobe-type-tools/cmap-resources)
(via pdf.js `external/bcmaps`). License details are in the `NOTICE` file at
the crate root.

## Encrypted PDFs

pdfni implements the PDF Standard security handler for revisions R2 – R6
with the `/V` values 1 / 2 / 4 / 5. This covers RC4 and AES-128 / AES-256.
The optional `password` argument on the byte-input entry points is passed to
the handler; an empty or missing password is tried first.

Errors:

- Wrong or missing password → `Error::BadPassword`
- Non-standard security handler → `Error::Reader("unknown encryption method: ...")`

## Resource limits

To defend against decompression bombs, the cumulative output of a stream's
filter chain (Flate, LZW, RunLength, and any combination) is capped at
512 MiB per stream by default. Exceeding the cap returns `Error::Reader`
instead of exhausting memory; partial decoding of corrupt streams still works
within the cap. Because the cap is cumulative, the stages of a multi-stage
chain share one budget — each stage can produce less than the full cap — and
as a side effect the transient peak memory while decoding one stream (the
previous stage's output plus the current one's) also stays within roughly the
cap. The cap is configurable via `ExtractOptions.reader
.max_decoded_bytes` (Rust and `options_json` on wasm), or via the
`ReaderSettings` argument of the `extract_text_from_bytes_with` /
`read_pages_with` entry points.

## Parallelism

On native, pages are processed in parallel (see `src/par.rs`). On wasm, pages
are processed sequentially. The output order and content match across both
targets — the wasm build is a strict subset of the native behavior. Note that
the decode cap above applies per stream, so peak memory scales with the
number of worker threads on native.

## Directory layout

```
src/
  reader/          pure-Rust PDF reader (+ data tables from pdf.js / Adobe CMaps)
  extract.rs       parts → table detection orchestration
  detect_*.rs      lattice / hybrid / text detectors
  text.rs          TextDoc, items, lines
  document.rs      DocDoc, headings/lists, Markdown
  columns.rs       multi-column layout for the document model
  wasm.rs          JS exports
  bin/main.rs      CLI (tables only)
examples/          reader_probe, text_probe, ...
test/wasm-test/    browser smoke page
data/              source .bcmap files used when regenerating bundled data
```
