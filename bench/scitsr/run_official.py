# SciTSR official evaluation for pdfni.
#
# Pipeline per document (single-table PDFs, 3,000 test docs):
#   1. pdfni CLI (../../target/release/pdfni) -> tables JSON
#   2. convert to SciTSR cell format; cell spans reconstructed from cell
#      bboxes (shared code: ../icdar2013-corrected/run_official.py); relations of all
#      detected tables in a document are merged
#   3. score with the official scitsr.eval code (tools/SciTSR):
#      json2Table / Table2Relations / compare_rel with cmp_blank=True
#   4. aggregate exactly like the official eval_relations: per-document P/R
#      averaged over all documents (macro), F1 = 2PR/(P+R); micro totals
#      are recorded for reference
#
# Usage: python3 run_official.py [--force] [--limit N] [--verbose]

import json
import os
import subprocess
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
DATA = os.path.join(HERE, "data", "test")
PDFNI = os.path.join(HERE, "..", "..", "target", "release", "pdfni")
WORK = os.path.join(HERE, "work")
RESULTS = os.path.join(HERE, "results")

sys.path.insert(0, os.path.join(HERE, "tools", "SciTSR"))
sys.path.insert(0, os.path.join(HERE, "..", "icdar2013-corrected"))

from scitsr.eval import json2Table, Table2Relations, compare_rel  # noqa: E402
from run_official import table_cells  # noqa: E402


def doc_relations(doc):
    """Detected tables -> official Relation list (merged across tables)."""
    rels = []
    for pg in doc["pages"]:
        for t in pg["tables"]:
            cells = [
                {"content": text, "start_row": r0, "end_row": r1,
                 "start_col": c0, "end_col": c1}
                for r0, r1, c0, c1, text in table_cells(t)
            ]
            if cells:
                rels.extend(Table2Relations(
                    json2Table({"cells": cells}, splitted_content=False)))
    return rels


def main():
    force = "--force" in sys.argv
    verbose = "--verbose" in sys.argv
    limit = None
    if "--limit" in sys.argv:
        limit = int(sys.argv[sys.argv.index("--limit") + 1])

    for path, hint in [
        (os.path.join(DATA, "pdf"), "place the SciTSR test split under data/"),
        (os.path.join(HERE, "tools", "SciTSR", "scitsr"),
         "clone the official repository to tools/SciTSR"),
        (PDFNI, "run: cargo build --release (in the crate root)"),
    ]:
        if not os.path.exists(path):
            sys.exit(f"missing {path} — {hint}")

    os.makedirs(WORK, exist_ok=True)
    os.makedirs(RESULTS, exist_ok=True)
    pdf_dir = os.path.join(DATA, "pdf")
    struct_dir = os.path.join(DATA, "structure")

    tot_prec = tot_rec = 0.0
    total = 0
    micro = [0, 0, 0]  # corr, res, gt
    per_doc = []
    errors = []

    names = sorted(f for f in os.listdir(pdf_dir) if f.endswith(".pdf"))
    if limit:
        names = names[:limit]
    for i, pdf_name in enumerate(names):
        stem = pdf_name[:-4]
        gt_path = os.path.join(struct_dir, stem + ".json")
        if not os.path.exists(gt_path):
            continue
        out_json = os.path.join(WORK, stem + ".json")
        if force or not os.path.exists(out_json):
            r = subprocess.run([PDFNI, "--output", "tables",
                                os.path.join(pdf_dir, pdf_name), out_json],
                               capture_output=True, text=True)
            if r.returncode != 0:
                json.dump({"source": pdf_name, "pages": []}, open(out_json, "w"))
                errors.append(f"{stem}: pdfni rc={r.returncode}")
        doc = json.load(open(out_json, encoding="utf-8"))

        gt_rels = Table2Relations(json2Table(
            json.load(open(gt_path, encoding="utf-8")), "", splitted_content=True))
        res_rels = doc_relations(doc)

        corr = compare_rel(gt_rels, res_rels, cmp_blank=True)
        prec = corr / len(res_rels) if res_rels else 0.0
        rec = corr / len(gt_rels) if gt_rels else 0.0
        tot_prec += prec
        tot_rec += rec
        total += 1
        micro[0] += corr
        micro[1] += len(res_rels)
        micro[2] += len(gt_rels)
        per_doc.append({"doc": stem, "corr": corr, "res": len(res_rels),
                        "gt": len(gt_rels)})
        if verbose and (i + 1) % 200 == 0:
            print(f"{i + 1}/{len(names)}", flush=True)

    p = tot_prec / total
    r = tot_rec / total
    f = 2 * p * r / (p + r) if p + r > 0 else 0.0
    mp = micro[0] / micro[1] if micro[1] else 0.0
    mr = micro[0] / micro[2] if micro[2] else 0.0
    mf = 2 * mp * mr / (mp + mr) if mp + mr > 0 else 0.0
    summary = {
        "documents": total,
        "official_macro": {"precision": round(p, 4), "recall": round(r, 4),
                           "f1": round(f, 4)},
        "micro_reference": {"corr": micro[0], "res": micro[1], "gt": micro[2],
                            "precision": round(mp, 4), "recall": round(mr, 4),
                            "f1": round(mf, 4)},
    }
    out = {"summary": summary, "errors": errors, "per_doc": per_doc}
    json.dump(out, open(os.path.join(RESULTS, "official_results.json"), "w",
                        encoding="utf-8"), indent=1)
    print(json.dumps(summary, indent=2))
    if errors:
        print(f"ERRORS: {len(errors)}")


if __name__ == "__main__":
    main()
