# ICDAR 2013 Table Competition benchmark — ORIGINAL ground truth

Scores pdfni with the competition's official Java evaluation tool against the
**original competition ground truth**
(`icdar2013-competition-dataset-with-gt.zip` from tamirhassan.com). This
directory is self-contained: only the GT under its own `data/` is used. The
corrected GT lives in the sibling directory `../icdar2013-corrected/`.

## What is measured

Full pipeline: each PDF goes through the pdfni CLI with default options,
and table regions are NOT given to the tool. Note that the original
competition's structure track supplied ground-truth regions, so published
2013 structure numbers were measured under an easier protocol.

- Region detection (`-reg`): character-level micro P/R/F1, plus the
  competition's completeness/purity counts.
- Structure recognition (`-str`): cell adjacency relation micro P/R/F1.
  Adjacency relations of detected tables that match no ground-truth table
  are counted in the precision denominator (`fp_ars`). The tool itself only
  prints per-table numbers for matched tables, so the matched-only
  aggregation is also recorded (`precision_matched_only`).

## Inputs (not committed)

- `data/competition-dataset-eu/`, `data/competition-dataset-us/` —
  original competition GT (PDFs + GT XML)
- `tools/dataset-tools-20180206.jar`
- `tools/dataset-tools/` — tamirhassan/dataset-tools clone (`lib/` jars are
  used at runtime)

## Running

```
(cd ../.. && cargo build --release)
python3 run_official.py                      # -> results/official_results.json
python3 run_official.py --norm2 --skip-reg   # -> results/official_results_norm2.json
```

Results are written to `results/`.
