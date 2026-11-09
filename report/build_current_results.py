"""Validate the separately identified model/coder and size experiments."""
import csv
from decimal import Decimal
import hashlib
import itertools
import json
from pathlib import Path
import re
import statistics

ROOT = Path(__file__).resolve().parent
RUN5 = ROOT / "data/shkarin-range-run5"
RUN21 = ROOT / "data/shkarin-run21"
BUILDS = ("new", "trie", "port")
FIVE = "bin6+par+inh+seef+inhd"
STAGES = {
    11: ("base", "par", "bin6", "inh", "see2", "bin6+par", "bin6+par+inh", "bin6+par+inh+see2"),
    13: ("base", "see2", "seef", "bin6+par+inh+see2", "bin6+par+inh+seef"),
    18: ("base", "bin6+par+inh+seef", "bin6+par+inh+seef+inhn", FIVE, "bin6+inhn", "bin6+inhd"),
    21: ("base", "ee1", FIVE, FIVE + "+ee1"),
}


def evidence(folder):
    manifest = json.loads((folder / "manifest.json").read_text())
    for name, wanted in manifest["files"].items():
        if not re.fullmatch(r"[0-9a-f]{64}", wanted):
            raise ValueError(f"Invalid evidence hash: {name}")
        if hashlib.sha256((folder / name).read_bytes()).hexdigest() != wanted:
            raise ValueError(f"Changed evidence: {name}")


def read_rows(path):
    with path.open(newline="") as stream:
        return list(csv.DictReader(stream, delimiter="\t"))


def validate_run5(rows, inputs, outputs):
    expected = set(itertools.product(inputs, ("1", "2", "3"), BUILDS))
    keys = [(r["file"], r["rep"], r["build"]) for r in rows]
    if len(inputs) != 12 or len(keys) != len(set(keys)) or set(keys) != expected:
        raise ValueError("Missing, duplicate, or unexpected run5 case")
    if set(outputs) != {f"{f}/{b}" for f in inputs for b in BUILDS}:
        raise ValueError("Incomplete output identities")
    for name in inputs:
        for rep in (1, 2, 3):
            actual = tuple(r["build"] for r in rows if r["file"] == name and r["rep"] == str(rep))
            wanted = BUILDS[rep - 1:] + BUILDS[:rep - 1]
            if actual != wanted:
                raise ValueError("Run5 Latin-square order changed")
    for row in rows:
        if (row["enc_status"], row["dec_status"], row["cmp"]) != ("0", "0", "ok"):
            raise ValueError("Failed run5 process or round trip")
        identity = outputs[f"{row['file']}/{row['build']}"]
        if not re.fullmatch(r"[0-9a-f]{64}", row["out_sha256"]):
            raise ValueError("Invalid output hash")
        if (int(row["bytes"]), row["out_sha256"]) != (identity["bytes"], identity["sha256"]):
            raise ValueError("Run5 output differs")
        for side in ("enc", "dec"):
            for field in ("user", "sys", "elapsed"):
                value = Decimal(row[f"{side}_{field}"])
                if not value.is_finite() or value < 0:
                    raise ValueError("Invalid run5 time")
            if int(row[f"{side}_rss_kib"]) <= 0:
                raise ValueError("Invalid process RSS")
    metrics = {}
    for build in BUILDS:
        selected = [r for r in rows if r["build"] == build]
        m = {"bytes": sum(outputs[f"{f}/{build}"]["bytes"] for f in inputs)}
        for side in ("enc", "dec"):
            for measure in ("cpu", "elapsed"):
                fields = ("user", "sys") if measure == "cpu" else ("elapsed",)
                totals = [sum(Decimal(r[f"{side}_{field}"]) for r in selected
                              if r["rep"] == str(rep) for field in fields) for rep in (1, 2, 3)]
                m[f"{side}_{measure}"] = statistics.median(totals)
            m[f"{side}_rss_kib"] = max(int(r[f"{side}_rss_kib"]) for r in selected)
        metrics[build] = m
    return metrics


def run5():
    evidence(RUN5)
    rows = read_rows(RUN5 / "rows.tsv")
    inputs = json.loads((RUN5 / "inputs.json").read_text())
    outputs = json.loads((RUN5 / "outputs.json").read_text())
    m = validate_run5(rows, inputs, outputs)
    cases = json.loads((RUN5 / "format13-cases.json").read_text())
    if len(cases) != 12 or {c["file"] for c in cases} != set(inputs):
        raise ValueError("Incomplete format13 identity cases")
    for c in cases:
        if (c["decode_status"] != 0 or c["input_sha256"] != inputs[c["file"]]["sha256"]
                or c["input_bytes"] != inputs[c["file"]]["bytes"]
                or c["bytes"] != outputs[f"{c['file']}/new"]["bytes"]
                or not re.fullmatch(r"[0-9a-f]{64}", c["output_sha256"])):
            raise ValueError("Invalid format13 identity case")
    return rows, inputs, m


def validate_run21(rows, inputs, outputs, configs=STAGES[21]):
    by_name = {Path(name).name: value for name, value in inputs.items()}
    if len(inputs) != 33 or len(by_name) != 33 or len(rows) != len(configs) or {r["config"] for r in rows} != set(configs):
        raise ValueError("Incomplete run21 matrix")
    expected = {f"./{c}/{name}" for c in configs for name in by_name}
    if set(outputs) != expected:
        raise ValueError("Incomplete run21 output identities")
    for value in inputs.values():
        if value["expected"] != value["actual"] or not re.fullmatch(r"[0-9a-f]{64}", value["actual"]):
            raise ValueError("Run21 input identity differs")
    for row in rows:
        if set(row) != {"config", "total", "encode_s", "verify_s"} | set(by_name):
            raise ValueError("Unexpected run21 columns")
        if sum(int(row[n]) for n in by_name) != int(row["total"]):
            raise ValueError("Run21 total differs")
        for name in by_name:
            value = outputs[f"./{row['config']}/{name}"]
            if (value["bytes"] != int(row[name]) or value["expected"] != value["actual"]
                    or not re.fullmatch(r"[0-9a-f]{64}", value["actual"])):
                raise ValueError("Run21 output identity differs")
    return {r["config"]: r for r in rows}


def run21():
    evidence(RUN21)
    return validate_run21(read_rows(RUN21 / "sizes.tsv"),
                          json.loads((RUN21 / "inputs.json").read_text()),
                          json.loads((RUN21 / "outputs.json").read_text()))


def stages():
    """Validate all configurations, including those not printed in the table.

    Shared configurations must have identical independently captured output
    hashes; differing source versions are never merged merely by equal sizes.
    """
    records, seen = {}, {}
    reference_inputs = json.loads((RUN21 / "inputs.json").read_text())
    for number, configs in STAGES.items():
        folder = ROOT / f"data/shkarin-run{number}"
        evidence(folder)
        inputs = json.loads((folder / "inputs.json").read_text())
        if inputs != reference_inputs:
            raise ValueError("Development input identities changed")
        outputs = json.loads((folder / "outputs.json").read_text())
        records[number] = validate_run21(read_rows(folder / "sizes.tsv"), inputs, outputs, configs)
        for name, value in outputs.items():
            identity = (value["bytes"], value["actual"])
            if name in seen and seen[name] != identity:
                raise ValueError("Shared development configuration output differs")
            seen[name] = identity
    return records


def write_tables(build):
    _, _, metrics = run5()
    fields = [("Compressed bytes", "bytes", ",d"),
              ("Encoding CPU (s)", "enc_cpu", ".2f"), ("Decoding CPU (s)", "dec_cpu", ".2f"),
              ("Encoding elapsed (s)", "enc_elapsed", ".2f"), ("Decoding elapsed (s)", "dec_elapsed", ".2f"),
              ("Maximum encoding peak RSS (KiB)", "enc_rss_kib", ",d"),
              ("Maximum decoding peak RSS (KiB)", "dec_rss_kib", ",d")]
    lines = [" & ".join([label] + [format(metrics[b][key], fmt) for b in BUILDS]) + r"\\"
             for label, key, fmt in fields]
    (build / "current-comparison.tex").write_text("\n".join(lines) + "\n")
    ablation = run21()
    labels = [("Earlier trie model", "base"), ("EE1 alone", "ee1"),
              ("Selected five changes", FIVE), ("Selected five plus EE1", FIVE + "+ee1")]
    lines = []
    for label, config in labels:
        row = ablation[config]
        total = int(row["total"])
        change = 100 * (Decimal(total) / Decimal(ablation["base"]["total"]) - 1)
        lines.append(f"{label} & {total:,} & {change:+.3f}" + r"\%\\")
    (build / "model-selection.tex").write_text("\n".join(lines) + "\n")
    records = stages()
    labels = [
        (11, "Earlier model", "base"),
        (11, "Parent half update (P)", "par"),
        (11, "Binary escape classes (B)", "bin6"),
        (11, "Inheritance into existing contexts (I)", "inh"),
        (11, "Class escape-probability learner (Q)", "see2"),
        (11, "B + P", "bin6+par"), (11, "B + P + I", "bin6+par+inh"),
        (11, "B + P + I + Q", "bin6+par+inh+see2"),
        (13, "Class escape-frequency learner (F)", "seef"),
        (13, "B + P + I + F", "bin6+par+inh+seef"),
        (18, "B + P + I + F + immediate new inheritance", "bin6+par+inh+seef+inhn"),
        (18, "B + P + I + F + delayed new inheritance", FIVE),
    ]
    lines = [f"{number} & {label} & {int(records[number][config]['total']):,}" + r"\\"
             for number, label, config in labels]
    (build / "component-ablation.tex").write_text("\n".join(lines) + "\n")


if __name__ == "__main__":
    print(json.dumps(run5()[2], default=str, indent=2))
    print({k: v["total"] for k, v in run21().items()})
