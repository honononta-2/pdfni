# CLI

The `pdfni` binary extracts tables from a PDF and writes them as JSON.
It requires the `reader` feature (enabled by default).

## Usage

```sh
cargo run --release -- [--mode auto|ruled|borderless] [--password-stdin] <input.pdf> [output.json]
```

- Omit `output.json` to print to stdout.
- For encrypted PDFs, pass `--password-stdin` and feed the password on stdin as
  a single line. Only the first line is read; the trailing `\n` or `\r\n` is
  stripped. Piping or redirection is required (an interactive TTY is rejected).

## Modes

The CLI mode names map to internal detectors:

| CLI mode | Detectors run | Internal names |
| --- | --- | --- |
| `auto` (default) | lattice + hybrid + text | `lattice → hybrid → text` |
| `ruled` | lattice only (needs ruled lines) | `lattice` |
| `borderless` | text only (needs text alignment) | `text` |

See [ARCHITECTURE.md](ARCHITECTURE.md) for what each detector does.

## Examples

```sh
# Auto mode, write to file
cargo run --release -- input.pdf output.json

# Ruled tables only, print to stdout
cargo run --release -- --mode ruled input.pdf

# Encrypted PDF with a password (fed on stdin)
printf %s 'secret' | cargo run --release -- --password-stdin input.pdf output.json

# Or from a file
cargo run --release -- --password-stdin input.pdf output.json < password.txt
```

## Output

The output is the same JSON shape returned by `extract_from_bytes` — a
`Document { source, pages: [Page { page_number, width, height, tables: [Table { extraction_method, bbox, n_rows, n_cols, data }] }], warnings: [...] }`.
Coordinates are top-down PDF points. The `warnings` field lists non-fatal
reader diagnostics and is omitted from the JSON when empty. See the API
reference on [docs.rs](https://docs.rs/pdfni) for the full type definitions.
