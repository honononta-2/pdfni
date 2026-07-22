# CLI

The `pdfni` binary extracts document content and tables from a PDF and writes
them as JSON. It requires the `document` feature (enabled by default).

## Usage

```sh
cargo run --release -- [--output content|tables|doc] [--mode auto|ruled|borderless] [--pages 1,3-5] [--region L,T,R,B] [--detect-header-footer] [--password-stdin] <input.pdf> [output.json]
```

- Omit `output.json` to print to stdout.
- `--pages` selects 1-based pages (`1,3-5,7` style). `--region` restricts
  extraction to a rectangle given as `L,T,R,B` in top-down display
  coordinates.
- `--detect-header-footer` marks repeating header / footer blocks with a
  `role` field (affects the `content` and `doc` outputs).
- For encrypted PDFs, pass `--password-stdin` and feed the password on stdin as
  a single line. Only the first line is read; the trailing `\n` or `\r\n` is
  stripped. Piping or redirection is required (an interactive TTY is rejected).

## Outputs

| `--output` | JSON root | Contents |
| --- | --- | --- |
| `content` (default) | `DocContent` | text blocks and tables in reading order, without per-glyph data |
| `tables` | `Document` | tables only — the same shape as `extract_from_bytes` |
| `doc` | `DocDoc` | full document model, including per-page `fonts` and `chars` |

## Modes

The CLI mode names map to internal detectors:

| CLI mode | Detectors run | Internal names |
| --- | --- | --- |
| `auto` (default) | lattice + text | `lattice → text` |
| `ruled` | lattice only (needs ruled lines) | `lattice` |
| `borderless` | text only (needs text alignment) | `text` |

See [ARCHITECTURE.md](ARCHITECTURE.md) for what each detector does.

## Examples

```sh
# Default: content view (text blocks + tables), write to file
cargo run --release -- input.pdf output.json

# Tables only
cargo run --release -- --output tables input.pdf output.json

# Full document model (includes fonts and chars)
cargo run --release -- --output doc input.pdf output.json

# Ruled tables only, print to stdout
cargo run --release -- --output tables --mode ruled input.pdf

# Encrypted PDF with a password (fed on stdin)
printf %s 'secret' | cargo run --release -- --password-stdin input.pdf output.json

# Or from a file
cargo run --release -- --password-stdin input.pdf output.json < password.txt
```

## Output shape

`content` prints `DocContent { pages: [{ index, width, height, blocks: [...] }], warnings: [...] }`.
Each block is either `{ type: "table", ... }` (the same `Table` shape as the
tables output) or `{ type: "text", kind, text, left, right, top, bottom, lines }`.

`tables` prints the same JSON shape returned by `extract_from_bytes` — a
`Document { source, pages: [Page { index, width, height, tables: [Table { extraction_method, bbox, n_rows, n_cols, data }] }], warnings: [...] }`.

`doc` prints `DocDoc`, which extends the block structure of `content` with
per-page `fonts` and `chars`.

The `index` field is the zero-based position of the page entry in the `pages`
array. Coordinates are top-down PDF points. The `warnings` field lists
non-fatal reader diagnostics and is omitted from the JSON when empty. See the
API reference on [docs.rs](https://docs.rs/pdfni) for the full type
definitions.
