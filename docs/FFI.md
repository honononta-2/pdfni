# C ABI (native)

pdfni exposes PDF byte-input extraction through a C ABI so that it can be
called from any language with a C FFI. The entry points are compiled only
when the opt-in `ffi` feature is enabled, and are not available on wasm.

## Building

```sh
cargo build --release --features ffi
```

The entry points are exported from the shared library under
`target/release/` (`libpdfni.so`, `libpdfni.dylib` or `pdfni.dll`). See
[BUILD.md](BUILD.md) for the feature layering.

## Functions

| Function | Output |
| --- | --- |
| `pdfni_extract_tables_json` | table `Document` JSON — the same shape as `extract_from_bytes` |
| `pdfni_extract_text_json` | `TextDoc` JSON |
| `pdfni_extract_text_lines_json` | JSON array with one entry per page, each an array of lines |
| `pdfni_free_string` | releases a string returned by the functions above |

```c
int32_t pdfni_extract_tables_json(
    const uint8_t *pdf_ptr, size_t pdf_len,
    const char *options_json, const char *password,
    char **out_json, char **out_error);

int32_t pdfni_extract_text_json(
    const uint8_t *pdf_ptr, size_t pdf_len,
    const char *options_json, const char *password,
    char **out_json, char **out_error);

int32_t pdfni_extract_text_lines_json(
    const uint8_t *pdf_ptr, size_t pdf_len,
    const char *options_json, const char *password,
    char **out_json, char **out_error);

void pdfni_free_string(char *ptr);
```

## Arguments

The three extraction functions take the same arguments.

| Argument | Required | Meaning |
| --- | --- | --- |
| `pdf_ptr`, `pdf_len` | yes | PDF bytes. A null pointer or a zero length fails with status `5` |
| `options_json` | no | NUL-terminated UTF-8 JSON. Null or an empty string selects the defaults |
| `password` | no | NUL-terminated UTF-8 password for encrypted PDFs. Null when not needed |
| `out_json` | yes | receives the result JSON on success, null on failure |
| `out_error` | no | receives the error message on failure, null on success. Pass null to ignore |

## `options_json`

`options_json` accepts the same JSON that `ExtractOptions` deserializes on the
Rust side, plus two keys that restrict the extraction range:

- `pages` — 1-based page selection as a string (`"1,3-5,7"` style).
- `region` — a rectangle in top-down display coordinates. `left` and `top`
  are required, together with either `right` / `bottom` or `width` /
  `height`. When both forms are given, `right` / `bottom` win, and the
  `Document` and `TextDoc` outputs report it in `warnings`.

```json
{ "mode": "ruled", "pages": "1,3-5", "region": { "left": 0, "top": 0, "width": 595, "height": 400 } }
```

`pdfni_extract_text_json` and `pdfni_extract_text_lines_json` only read the
`reader` section, `pages` and `region`.

## Status codes

Each extraction function returns a status code.

| Status | Meaning |
| --- | --- |
| `0` | success |
| `1` | the password is required or incorrect |
| `2` | invalid page selection |
| `3` | PDF security restrictions prevent loading |
| `4` | invalid PDF format |
| `5` | any other failure, including invalid arguments, malformed `options_json` and reader errors |

A panic inside pdfni is caught at the boundary and reported as status `5`.

## Memory

Strings written to `out_json` and `out_error` are owned by the caller and must
be released with `pdfni_free_string`. Passing null to `pdfni_free_string` is a
no-op. The input buffers are only read during the call.
