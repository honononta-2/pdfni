# SciTSR benchmark (official evaluation code)

Scores pdfni on the SciTSR test split with the dataset's official
evaluation code. Full pipeline: each PDF goes through the pdfni CLI with
default options; relations of all detected tables in a document are merged
(the documents are single-table). Scoring and aggregation follow the
official `scitsr.eval` exactly: per-document precision/recall of cell
adjacency relations (`cmp_blank=True`), averaged over all documents
(macro); F1 = 2PR/(P+R). Micro totals are recorded for reference only.

## Inputs (not committed)

- `data/test/pdf/`, `data/test/structure/` — SciTSR test split
- `tools/SciTSR/` — Academic-Hammer/SciTSR clone (its `scitsr/` package is
  imported for scoring)

## Running

```
(cd ../.. && cargo build --release)
python3 run_official.py          # -> results/official_results.json
```

Results are written to `results/official_results.json`.
