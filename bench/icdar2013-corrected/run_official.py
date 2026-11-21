# ICDAR 2013 Table Competition scoring with the official evaluation tool.
#
# This directory is self-contained: it scores ONLY the ground truth stored
# under its own data/ directory. See README.md for which GT variant that is.
#
# Pipeline per document (full pipeline: table regions are NOT given):
#   1. pdfni CLI (../../target/release/pdfni) -> tables JSON
#   2. convert JSON -> <stem>-reg-result.xml / <stem>-str-result.xml
#      (competition entry format; cell spans reconstructed from cell bboxes:
#       a merged cell carries the union bbox, covered grid slots have a zero bbox)
#   3. official tool (dataset-tools) -reg / -str per document
#   4. aggregate micro P/R/F1 from the tool's stdout
#      - reg: character-level (correct items / result items + FP items / GT items)
#      - str: adjacency relations (corrDet / detected + FP ARs / GT size)
#
# Prerequisites: cargo build --release (crate root), java on PATH.
#
# Usage: python3 run_official.py [--only STEM] [--force] [--skip-reg]
#                                [--skip-str] [--norm2] [--verbose]
#   --force    re-run extraction even if a cached work/ JSON exists
#   --norm2    pass -ignorechars2 to the tool (lenient content normalization);
#              adds a _norm2 suffix to the results file name

import json
import os
import re
import subprocess
import sys
from xml.sax.saxutils import escape

HERE = os.path.dirname(os.path.abspath(__file__))
DATA = os.path.join(HERE, "data")
SETS = ["competition-dataset-eu", "competition-dataset-us"]
PDFNI = os.path.join(HERE, "..", "..", "target", "release", "pdfni")
JAR = os.path.join(HERE, "tools", "dataset-tools-20180206.jar")
LIB = os.path.join(HERE, "tools", "dataset-tools", "lib")
MAIN_CLASS = "at.ac.tuwien.dbai.pdfwrap.MeasureRecognitionPerformance"
WORK = os.path.join(HERE, "work")
LOGS = os.path.join(HERE, "logs")
RESULTS = os.path.join(HERE, "results")

EXTRA_ARGS = []


# ---------- extraction ----------

def extract(pdf, out_json):
    r = subprocess.run([PDFNI, "--output", "tables", pdf, out_json],
                       capture_output=True, text=True)
    if r.returncode != 0:
        json.dump({"source": pdf, "pages": []}, open(out_json, "w"))
        return f"pdfni failed: {r.stderr.strip()[:200]}"
    return None


# ---------- span reconstruction ----------

def nonzero(b):
    return (b["right"] - b["left"]) > 0 and (b["bottom"] - b["top"]) > 0


def axis_intervals(table, axis):
    """Per-column (axis=0) or per-row (axis=1) interval, taken as the
    narrowest non-zero cell bbox in that column/row (span anchors are wider)."""
    n = table["n_cols"] if axis == 0 else table["n_rows"]
    ivals = [None] * n
    for r, row in enumerate(table["data"]):
        for c, cell in enumerate(row):
            b = cell["bbox"]
            if not nonzero(b):
                continue
            i = c if axis == 0 else r
            lo, hi = (b["left"], b["right"]) if axis == 0 else (b["top"], b["bottom"])
            if ivals[i] is None or (hi - lo) < (ivals[i][1] - ivals[i][0]):
                ivals[i] = (lo, hi)
    return ivals


def covered_run(ivals, idx, lo, hi):
    """Contiguous run of indices around idx whose interval centre falls in [lo,hi]."""
    def inside(i):
        if ivals[i] is None:
            return False
        c = (ivals[i][0] + ivals[i][1]) / 2.0
        return lo <= c <= hi

    start = end = idx
    while start - 1 >= 0 and inside(start - 1):
        start -= 1
    while end + 1 < len(ivals) and inside(end + 1):
        end += 1
    return start, end


def table_cells(table):
    """-> list of (r0, r1, c0, c1, text) for non-blank cells, spans from bboxes."""
    data = table["data"]
    col_iv = axis_intervals(table, 0)
    row_iv = axis_intervals(table, 1)
    cells = []
    for r, row in enumerate(data):
        for c, cell in enumerate(row):
            text = cell["text"]
            if not "".join(text.split()):
                continue
            b = cell["bbox"]
            c0 = c1 = c
            r0 = r1 = r
            if nonzero(b):
                c0, c1 = covered_run(col_iv, c, b["left"], b["right"])
                r0, r1 = covered_run(row_iv, r, b["top"], b["bottom"])
            # clamp: a span may not swallow another non-blank cell
            for cc in range(c - 1, c0 - 1, -1):
                if "".join(data[r][cc]["text"].split()):
                    c0 = cc + 1
                    break
            for cc in range(c + 1, c1 + 1):
                if "".join(data[r][cc]["text"].split()):
                    c1 = cc - 1
                    break
            for rr in range(r - 1, r0 - 1, -1):
                if "".join(data[rr][c]["text"].split()):
                    r0 = rr + 1
                    break
            for rr in range(r + 1, r1 + 1):
                if "".join(data[rr][c]["text"].split()):
                    r1 = rr - 1
                    break
            cells.append((r0, r1, c0, c1, text))
    return cells


# ---------- result XML ----------

def write_reg_xml(doc, path):
    lines = ['<?xml version="1.0" encoding="UTF-8"?>', "<document>"]
    tid = 0
    for pg in doc["pages"]:
        h = pg["height"]
        for t in pg["tables"]:
            tid += 1
            b = t["bbox"]
            x1, y1 = b["left"], h - b["bottom"]
            x2, y2 = b["right"], h - b["top"]
            lines.append(f'  <table id="{tid}">')
            lines.append(f'    <region page="{pg["index"] + 1}">')
            lines.append(
                f'      <bounding-box x1="{x1:.2f}" y1="{y1:.2f}" x2="{x2:.2f}" y2="{y2:.2f}"/>'
            )
            lines.append("    </region>")
            lines.append("  </table>")
    lines.append("</document>")
    open(path, "w", encoding="utf-8").write("\n".join(lines))


def write_str_xml(doc, path):
    lines = ['<?xml version="1.0" encoding="UTF-8"?>', "<document>"]
    tid = 0
    for pg in doc["pages"]:
        for t in pg["tables"]:
            tid += 1
            lines.append(f'  <table id="{tid}">')
            lines.append(
                f'    <region page="{pg["index"] + 1}" col-increment="0" row-increment="0">'
            )
            for r0, r1, c0, c1, text in table_cells(t):
                attrs = (
                    f'start-row="{r0}" start-col="{c0}" end-row="{r1}" end-col="{c1}"'
                )
                lines.append(
                    f"      <cell {attrs}><content>{escape(text)}</content></cell>"
                )
            lines.append("    </region>")
            lines.append("  </table>")
    lines.append("</document>")
    open(path, "w", encoding="utf-8").write("\n".join(lines))


# ---------- official tool ----------

def classpath():
    jars = [JAR] + sorted(
        os.path.join(LIB, j) for j in os.listdir(LIB) if j.endswith(".jar")
    )
    return ":".join(jars)


def run_tool(mode, gt_xml, result_xml, pdf, log_path):
    cmd = ["java", "-cp", classpath(), MAIN_CLASS, mode, *EXTRA_ARGS,
           gt_xml, result_xml, pdf]
    r = subprocess.run(cmd, capture_output=True, text=True, timeout=600)
    open(log_path, "w", encoding="utf-8").write(
        "CMD: " + " ".join(cmd)
        + "\n=== STDOUT ===\n" + r.stdout + "\n=== STDERR ===\n" + r.stderr
    )
    return r


RE_REG_MATCHED = re.compile(
    r"Region: (\d+)\s+GT Items: (\d+)\s+correct items: (\d+)\s+result items: (\d+)"
)
RE_REG_UNMATCHED = re.compile(r"Region: (\d+)\s+GT Items: (\d+)\s+complete:")
RE_REG_COMPLETE = re.compile(r"complete: (\w+)\s+pure: (\S+)")
RE_FP_ITEMS = re.compile(r"FP items: (\d+)")
RE_STR_TABLE = re.compile(
    r"Table (\d+):\s+GT size: (\d+) corrDet: (\d+) detected: (\d+)"
)
RE_STR_NOMATCH = re.compile(r"Table (\d+): no matching result found")
RE_STR_FP = re.compile(r"FP table with (\d+) adjacency relations")


def parse_reg(stdout):
    gt_items = correct = result_items = fp_items = 0
    complete = pure = regions = detected = 0
    for line in stdout.splitlines():
        m = RE_REG_MATCHED.search(line)
        if m:
            regions += 1
            detected += 1
            gt_items += int(m.group(2))
            correct += int(m.group(3))
            result_items += int(m.group(4))
        else:
            m2 = RE_REG_UNMATCHED.search(line)
            if m2:
                regions += 1
                gt_items += int(m2.group(2))
        mc = RE_REG_COMPLETE.search(line)
        if mc:
            if mc.group(1) == "true":
                complete += 1
            if mc.group(2) == "true":
                pure += 1
        mf = RE_FP_ITEMS.search(line)
        if mf:
            fp_items += int(mf.group(1))
    return {
        "gt_regions": regions,
        "matched": detected,
        "gt_items": gt_items,
        "correct_items": correct,
        "result_items": result_items,
        "fp_items": fp_items,
        "complete": complete,
        "pure": pure,
    }


def parse_str(stdout):
    gt = corr = det = fp = tables = 0
    for line in stdout.splitlines():
        m = RE_STR_TABLE.search(line)
        if m:
            tables += 1
            gt += int(m.group(2))
            corr += int(m.group(3))
            det += int(m.group(4))
        elif RE_STR_NOMATCH.search(line):
            tables += 1
        else:
            mf = RE_STR_FP.search(line)
            if mf:
                fp += int(mf.group(1))
    return {"gt_tables": tables, "gt_ars": gt, "correct_ars": corr,
            "detected_ars": det, "fp_ars": fp}


def prf(tp, det, gt):
    p = tp / det if det else 0.0
    r = tp / gt if gt else 0.0
    f = 2 * p * r / (p + r) if p + r > 0 else 0.0
    return round(p, 4), round(r, 4), round(f, 4)


def main():
    global EXTRA_ARGS
    only = None
    if "--only" in sys.argv:
        only = sys.argv[sys.argv.index("--only") + 1]
    force = "--force" in sys.argv
    do_reg = "--skip-reg" not in sys.argv
    do_str = "--skip-str" not in sys.argv
    verbose = "--verbose" in sys.argv
    name_parts = ["official_results"]
    if "--norm2" in sys.argv:
        EXTRA_ARGS = ["-ignorechars2"]
        name_parts.append("norm2")
    results_name = "_".join(name_parts) + ".json"

    for path, hint in [
        (os.path.join(DATA, SETS[0]), "dataset missing under data/"),
        (JAR, "tools/ missing"),
        (PDFNI, "run: cargo build --release (in the crate root)"),
    ]:
        if not os.path.exists(path):
            sys.exit(f"missing {path} — {hint}")

    os.makedirs(WORK, exist_ok=True)
    os.makedirs(LOGS, exist_ok=True)
    os.makedirs(RESULTS, exist_ok=True)

    reg_tot = {}
    str_tot = {}
    errors = []
    per_doc = []

    for ds in SETS:
        d = os.path.join(DATA, ds)
        for pdf_name in sorted(f for f in os.listdir(d) if f.endswith(".pdf")):
            stem = pdf_name[:-4]
            if only and stem != only:
                continue
            pdf = os.path.join(d, pdf_name)
            gt_reg = os.path.join(d, stem + "-reg.xml")
            gt_str = os.path.join(d, stem + "-str.xml")
            if not os.path.exists(gt_reg):
                continue
            out_json = os.path.join(WORK, f"{ds}__{stem}.json")
            if force or not os.path.exists(out_json):
                err = extract(pdf, out_json)
                if err:
                    errors.append(f"{stem}: {err}")
            doc = json.load(open(out_json, encoding="utf-8"))

            reg_xml = os.path.join(WORK, f"{ds}__{stem}-reg-result.xml")
            str_xml = os.path.join(WORK, f"{ds}__{stem}-str-result.xml")
            write_reg_xml(doc, reg_xml)
            write_str_xml(doc, str_xml)

            entry = {"doc": f"{ds}/{stem}"}
            if do_reg:
                try:
                    r = run_tool("-reg", gt_reg, reg_xml, pdf,
                                 os.path.join(LOGS, f"{ds}__{stem}-reg.log"))
                    stats = parse_reg(r.stdout)
                    entry["reg"] = stats
                    for k, v in stats.items():
                        reg_tot[k] = reg_tot.get(k, 0) + v
                    if r.returncode != 0:
                        errors.append(f"{stem}: reg tool rc={r.returncode}")
                except Exception as e:
                    errors.append(f"{stem}: reg tool error {e}")
            if do_str and os.path.exists(gt_str):
                try:
                    r = run_tool("-str", gt_str, str_xml, pdf,
                                 os.path.join(LOGS, f"{ds}__{stem}-str.log"))
                    stats = parse_str(r.stdout)
                    entry["str"] = stats
                    for k, v in stats.items():
                        str_tot[k] = str_tot.get(k, 0) + v
                    if r.returncode != 0:
                        errors.append(f"{stem}: str tool rc={r.returncode}")
                except Exception as e:
                    errors.append(f"{stem}: str tool error {e}")
            per_doc.append(entry)
            if verbose:
                print(f"done {ds}/{stem}", flush=True)

    summary = {}
    if reg_tot:
        p, r, f = prf(reg_tot["correct_items"],
                      reg_tot["result_items"] + reg_tot["fp_items"],
                      reg_tot["gt_items"])
        summary["region_char_level"] = {
            **reg_tot, "precision": p, "recall": r, "f1": f}
    if str_tot:
        p, r, f = prf(str_tot["correct_ars"],
                      str_tot["detected_ars"] + str_tot["fp_ars"],
                      str_tot["gt_ars"])
        pm, rm, fm = prf(str_tot["correct_ars"], str_tot["detected_ars"],
                         str_tot["gt_ars"])
        summary["structure_adjacency"] = {
            **str_tot, "precision": p, "recall": r, "f1": f,
            "precision_matched_only": pm, "f1_matched_only": fm}
    out = {"summary": summary, "errors": errors, "per_doc": per_doc}
    json.dump(out, open(os.path.join(RESULTS, results_name), "w",
                        encoding="utf-8"), indent=1)
    print(json.dumps(summary, indent=2))
    if errors:
        print("ERRORS:")
        for e in errors:
            print(" ", e)


if __name__ == "__main__":
    main()
