"""Validate the held-out Census and Backblaze size measurements."""
import csv
from decimal import Decimal
from pathlib import Path
import re

import build_current_results

ROOT = Path(__file__).resolve().parent
HELDOUT = ROOT / "data/heldout"
CODECS = ("acz4", "xz9", "zstd19", "acz3_0.5.0", "acz3")
# Collection: (label, measured inputs, empty inputs measured by no codec).
COLLECTIONS = {"census": ("Census", 11_058, 1), "backblaze": ("Backblaze", 688, 0)}


def read_tsv(name):
    with (HELDOUT / name).open(newline="") as stream:
        return list(csv.DictReader(stream, delimiter="\t"))


def read_rows(collection):
    """One row per input. Level 4 is release 0.7.0's, from its own pass over
    the sample; the earlier level-4 size is kept as acz4_0.6.0."""
    rows = read_tsv(f"{collection}-files.tsv")
    return with_level_four(rows, read_tsv(f"{collection}-acz4-v14.tsv"))


def with_level_four(rows, new):
    """Replace each row's level-4 size by the later pass's, which must have
    measured the same inputs with the same content."""
    by_id = {r["path_id"]: r for r in new}
    if len(by_id) != len(new) or set(by_id) != {r["path_id"] for r in rows}:
        raise ValueError("The level-4 pass measured different inputs")
    out = []
    for r in rows:
        n = by_id[r["path_id"]]
        if (n["content_bytes"], n["content_sha256"], n["status"]) != (r["content_bytes"], r["content_sha256"], r["status"]):
            raise ValueError(f"The level-4 pass differs: {r['path_id']}")
        out.append({**r, "acz4": n["acz4"], "acz4_0.6.0": r["acz4"]})
    return out


def validate(rows, measured, empty):
    """Return the inputs every codec measured; reject anything else."""
    ids = [r["path_id"] for r in rows]
    if len(set(ids)) != len(ids):
        raise ValueError("Duplicate input")
    ok, empties = [], 0
    for r in rows:
        if not re.fullmatch(r"[0-9a-f]{64}", r["content_sha256"]):
            raise ValueError(f"Invalid content hash: {r['path_id']}")
        if r["status"] == "EMPTY" and r["content_bytes"] == "0":
            empties += 1
        elif r["status"] == "ok":
            sizes = [r["content_bytes"]] + [r[c] for c in CODECS]
            if not all(s.isdigit() and int(s) > 0 for s in sizes):
                raise ValueError(f"Invalid size: {r['path_id']}")
            ok.append(r)
        else:
            raise ValueError(f"Unmeasured input: {r['path_id']} {r['status']}")
    if (len(ok), empties) != (measured, empty):
        raise ValueError(f"Expected {measured} measured and {empty} empty inputs")
    return ok


def totals(collection):
    _, measured, empty = COLLECTIONS[collection]
    rows = validate(read_rows(collection), measured, empty)
    content = sum(int(r["content_bytes"]) for r in rows)
    return len(rows), content, {c: sum(int(r[c]) for r in rows) for c in CODECS}


def percent(part, whole):
    return 100 * Decimal(part) / Decimal(whole)


def write_tables(build):
    build_current_results.evidence(HELDOUT)
    lines, macros = [], {}
    for collection, (label, _, _) in COLLECTIONS.items():
        n, content, packed = totals(collection)
        gb = Decimal(content) / Decimal(10**9)
        cells = [f"{percent(packed[c], content):.2f}" for c in CODECS]
        lines.append(f"{label} & {n:,} & {gb:.1f} & " + " & ".join(cells) + r"\\")
        macros[f"{label}Files"] = f"{n:,}"
        macros[f"{label}GB"] = f"{gb:.1f}"
        for c, name in zip(CODECS, ("CM", "Xz", "Zstd", "Port", "Ppm")):
            macros[f"{label}{name}"] = f"{percent(packed[c], content):.2f}"
        macros[f"{label}PpmLargerPort"] = f"{percent(packed['acz3'], packed['acz3_0.5.0']) - 100:.1f}"
        macros[f"{label}CMSmallerXz"] = f"{100 - percent(packed['acz4'], packed['xz9']):.1f}"
        old = sum(int(r["acz4_0.6.0"]) for r in validate(read_rows(collection), *COLLECTIONS[collection][1:]))
        macros[f"{label}CMOld"] = f"{percent(old, content):.2f}"
    (build / "heldout.tex").write_text("\n".join(lines) + "\n")
    (build / "heldout-metrics.tex").write_text(
        "\n".join(rf"\newcommand{{\{k}}}{{{v}}}" for k, v in macros.items()) + "\n")
    return macros


if __name__ == "__main__":
    for collection in COLLECTIONS:
        n, content, packed = totals(collection)
        print(collection, n, content, {c: f"{percent(v, content):.2f}" for c, v in packed.items()})
