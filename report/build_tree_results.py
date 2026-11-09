#!/usr/bin/env python3
"""Validate the frozen v9/v4 matrix and generate its separate report table.

These results describe a development experiment, not the v12 integration.
Historical report data and the original table generator are left intact.
"""
import csv
from decimal import Decimal
import hashlib
import itertools
import json
from pathlib import Path
import statistics

ROOT = Path(__file__).resolve().parent
DATA = ROOT / "data" / "trie-v4-run3"


def collect():
    manifest = json.loads((DATA / "manifest.json").read_text())
    for name, wanted in manifest["files"].items():
        if hashlib.sha256((DATA / name).read_bytes()).hexdigest() != wanted:
            raise ValueError(f"Changed evidence: {name}")
    inputs = json.loads((DATA / "inputs.json").read_text())
    with (DATA / "rows.tsv").open(newline="") as f:
        rows = list(csv.DictReader(f, delimiter="\t"))
    builds = ("v9", "tree4")
    expected = set(itertools.product(inputs, range(1, 5), builds))
    keys = [(r["file"], int(r["rep"]), r["build"]) for r in rows]
    if len(inputs) != 12 or len(keys) != len(set(keys)) or set(keys) != expected:
        raise ValueError("Missing, duplicate, or unexpected matrix case")
    orders = {1: builds, 2: builds[::-1], 3: builds[::-1], 4: builds}
    first = dict.fromkeys(builds, 0)
    for name in inputs:
        for rep in range(1, 5):
            pair = [r["build"] for r in rows if r["file"] == name and int(r["rep"]) == rep]
            if tuple(pair) != orders[rep]:
                raise ValueError(f"Unexpected pair order: {name}, {rep}")
            first[pair[0]] += 1
    if first != {"v9": 24, "tree4": 24}:
        raise ValueError("Unbalanced first positions")
    for row in rows:
        if (row["enc_status"], row["dec_status"], row["cmp"]) != ("0", "0", "ok"):
            raise ValueError(f"Failed process or round trip: {row}")
        wanted = inputs[row["file"]][row["build"]]
        if (int(row["bytes"]), row["out_sha256"]) != (wanted["bytes"], wanted["sha256"]):
            raise ValueError(f"Unexpected output: {row}")
        for direction in ("enc", "dec"):
            for metric in ("user", "sys", "elapsed"):
                value = Decimal(row[f"{direction}_{metric}"])
                if not value.is_finite() or value < 0:
                    raise ValueError(f"Invalid time: {row}")
            if int(row[f"{direction}_rss_kib"]) <= 0:
                raise ValueError(f"Invalid peak RSS: {row}")
            row[f"{direction}_cpu"] = Decimal(row[f"{direction}_user"]) + Decimal(row[f"{direction}_sys"])
    metrics = {b: {"bytes": sum(v[b]["bytes"] for v in inputs.values())} for b in builds}
    for build in builds:
        selected = [r for r in rows if r["build"] == build]
        for direction in ("enc", "dec"):
            for metric in ("cpu", "elapsed"):
                key = f"{direction}_{metric}"
                totals = [sum(Decimal(r[key]) for r in selected if int(r["rep"]) == rep) for rep in range(1, 5)]
                metrics[build][key] = statistics.median(totals)
            metrics[build][f"{direction}_rss_kib"] = max(int(r[f"{direction}_rss_kib"]) for r in selected)
    return metrics


def main():
    m = collect()
    build = ROOT / "build"
    build.mkdir(exist_ok=True)
    fields = [
        ("Encoding CPU (s)", "enc_cpu", ".3f"),
        ("Decoding CPU (s)", "dec_cpu", ".3f"),
        ("Encoding elapsed (s)", "enc_elapsed", ".3f"),
        ("Decoding elapsed (s)", "dec_elapsed", ".3f"),
        ("Maximum encoding peak RSS (KiB)", "enc_rss_kib", ",d"),
        ("Maximum decoding peak RSS (KiB)", "dec_rss_kib", ",d"),
        ("Compressed bytes", "bytes", ",d"),
    ]
    lines = ["% Frozen Silesia run3: experimental v4, not v12 integration timing.",
             r"\begin{tabular}{@{}lrr@{}}", r"\toprule",
             r"Measure & Original v9 & Forward trie v4\\", r"\midrule"]
    for label, key, fmt in fields:
        lines.append(f"{label} & {format(m['v9'][key], fmt)} & {format(m['tree4'][key], fmt)}" + r"\\")
    lines += [r"\bottomrule", r"\end{tabular}"]
    (build / "tree-v4-table.tex").write_text("\n".join(lines) + "\n")
    macros = []
    for label, key in [("Encode", "enc_cpu"), ("Decode", "dec_cpu")]:
        reduction = 100 * (1 - m["tree4"][key] / m["v9"][key])
        macros.append(r"\newcommand{\Tree" + label + "Reduction}{" + f"{reduction:.3f}" + "}")
    (build / "tree-v4-metrics.tex").write_text("\n".join(macros) + "\n")
    print(json.dumps({"cases": 96, "balanced_pairs": {"v9_first": 24, "tree4_first": 24},
                      "corpus_metrics": m}, default=str, indent=2))


if __name__ == "__main__":
    main()
