#!/usr/bin/env python3
"""Validate benchmark evidence, generate tables, and typeset report.tex.

The paper is edited in report.tex. This script never rewrites its prose or
runs benchmarks. Requires Python 3 and pdflatex; no Python packages.
"""
import csv
import hashlib
import itertools
import json
import re
import shutil
import statistics
import subprocess
from decimal import Decimal
from pathlib import Path

ROOT = Path(__file__).resolve().parent
DATA = ROOT / "data"
BUILD = ROOT / "build"
OUTPUT = ROOT / "output" / "pdf" / "arith-acz-technical-report-draft.pdf"
PPMD_RUN = "ppmd-v10-debc62e"
# Release 0.7.0 (format 14), every acz configuration, timed in one run.
V14_RUN = "silesia-v14-af3e950-run2"
V14_CODECS = {"acz14 -4": "acz4", "acz14 -3": "acz3", "acz14 -2": "acz2", "acz14 -1": "acz1",
              "acz14 -0": "acz0", "acz14 -4 --dedup": "acz4dedup"}
TREE_RUN = "trie-v4-run3"
TREE_BLOCKS = "trie-v4-blocks"
REPETITIONS = ("1", "2", "3")
# The original v9 experiment, whose level 3 was the hashed PPM.
HISTORICAL_LABELS = {
    "acz4": "acz -4 (CM)",
    "acz4dedup": "acz -4 + repeats (raw commands)",
    "xz9": "xz -9",
    "acz3": "acz -3 (PPM, hashed contexts)",
    "zstd19": "zstd -19",
    "bzip2_9": "bzip2 -9",
    "gzip9": "gzip -9",
    "acz2": "acz -2",
    "acz1": "acz -1",
    "acz0": "acz -0",
}
# The report's configurations: every acz row is release 0.7.0, from V14_RUN;
# the other compressors keep their rows, and 7-Zip's PPMd is a comparator
# from the format-10 experiment.
LABELS = dict(HISTORICAL_LABELS, acz3="acz -3 (PPM)", acz4dedup="acz -4 + repeats", ppmd7z="7-Zip PPMd -o6")
TREE_REPETITIONS = ("1", "2", "3", "4")
# Source snapshots that contain the PPMd-H port acz no longer includes. The
# public repository omits them; where present, their hashes are checked.
OPTIONAL_ARCHIVES = ("dedup-v11-69315de/source.tar.gz",)
DESCRIPTIONS = {
    "dickens": "English text",
    "mozilla": "Application files",
    "mr": "MR image",
    "nci": "Chemical database",
    "ooffice": "Executable",
    "osdb": "Database",
    "reymont": "Polish text (PDF)",
    "samba": "Source code",
    "sao": "Star catalog",
    "webster": "Dictionary (HTML)",
    "x-ray": "X-ray image",
    "xml": "XML",
}


def read_rows(name):
    with (DATA / name).open(newline="", encoding="utf-8") as source:
        return list(csv.DictReader(source, delimiter="\t"))


def historical_rows():
    """Retain the complete v9 experiment, including its final custom PPM run."""
    rows = [r for r in read_rows("acz.tsv") if r["codec"] != "acz3"]
    rows += read_rows("ppm.tsv")
    rows += [r for r in read_rows("comparison-source.tsv") if not r["codec"].startswith("acz")]
    return rows


def v14_rows(historical, sizes):
    """The 0.7.0 run in the original schema, one row per attempt.

    The run's script stops at the first failed exit status or round trip and
    writes DONE after the last configuration, so a complete file means every
    attempt succeeded. It records user plus system time as one number, which
    is kept in the user column. Its corpus manifest, identified in env.txt,
    must name the inputs of the original experiment.
    """
    folder = DATA / V14_RUN
    env = (folder / "env.txt").read_text()
    for name in ("bench.sh", "silesia.sha256"):
        if hashlib.sha256((folder / name).read_bytes()).hexdigest() not in env:
            raise ValueError(f"0.7.0 run: {name} is not the file the run recorded")
    hashes = {r["file"]: r["sha256"] for r in historical}
    manifest = dict(reversed(line.split()) for line in (folder / "silesia.sha256").read_text().splitlines())
    if manifest != hashes:
        raise ValueError("0.7.0 run: corpus differs from the original experiment")
    lines = (folder / "rows.tsv").read_text().splitlines()
    if lines[-1] != "DONE" or (folder / "err.log").read_text():
        raise ValueError("0.7.0 run is incomplete")
    rows, totals = [], {}
    for line in lines[:-1]:
        cells = line.split("\t")
        codec = V14_CODECS[cells[0]]
        if cells[1] == "TOTAL":
            totals[codec] = int(cells[2])
            continue
        _, name, packed, _, _, enc, dec = cells
        enc, dec = enc.split(","), dec.split(",")
        if len(enc) != len(REPETITIONS) or len(dec) != len(REPETITIONS):
            raise ValueError(f"0.7.0 run: wrong repetition count: {line}")
        for rep, e, d in zip(REPETITIONS, enc, dec):
            rows.append(dict(rep=rep, file=name, codec=codec, bytes=str(sizes[name]), sha256=hashes[name],
                             packed=packed, enc_user=e, enc_sys="0", enc_status="0",
                             dec_user=d, dec_sys="0", dec_status="0", cmp="ok"))
    for codec in V14_CODECS.values():
        if totals.get(codec) != sum(int(r["packed"]) for r in rows if r["codec"] == codec and r["rep"] == "1"):
            raise ValueError(f"0.7.0 run: total differs for {codec}")
    return rows


def selected_rows():
    """Every acz configuration from the 0.7.0 run, 7-Zip's PPMd from the
    format-10 run, and the other compressors from the original experiment."""
    historical = historical_rows()
    sizes = validate(historical, HISTORICAL_LABELS)
    v10 = validate_ppmd_v10(read_rows(f"{PPMD_RUN}/rows.tsv"), read_rows(f"{PPMD_RUN}/restarts.tsv"), historical, sizes)
    reference = [dict(r, codec="ppmd7z") for r in v10 if r["codec"] == "ppmd-o6-256m"]
    # Keep validating the earlier trie comparison without selecting its rows
    # as the current model's timing.
    tree_rows(historical, sizes)
    import build_current_results
    current, inputs, _ = build_current_results.run5()
    hashes = {r["file"]: r["sha256"] for r in historical}
    if {n: (v["bytes"], v["sha256"]) for n, v in inputs.items()} != {n: (sizes[n], hashes[n]) for n in sizes}:
        raise ValueError("Run5 input identities differ from the corpus")
    # Levels 0 to 3 are unchanged since run 5 and the original experiment,
    # so their sizes must be too, less nothing: the header is the same length.
    new = v14_rows(historical, sizes)
    packed = {(r["file"], r["codec"]): r["packed"] for r in new}
    for r in historical:
        if r["codec"] in ("acz0", "acz1", "acz2") and packed[r["file"], r["codec"]] != r["packed"]:
            raise ValueError(f"0.7.0 changed the size of {r['file']} at {r['codec']}")
    for r in current:
        if r["build"] == "new" and packed[r["file"], "acz3"] != r["bytes"]:
            raise ValueError(f"0.7.0 changed the level-3 size of {r['file']}")
    return [r for r in historical if not r["codec"].startswith("acz")] + new + reference


def repetitions(codec):
    return REPETITIONS


def tree_rows(historical, sizes):
    """The trie's rows of the balanced v9/trie run, in the original schema.

    Codex's collector checks the evidence hashes, the balanced order, every
    status and round trip, and each output against its recorded identity.
    """
    import build_tree_results
    build_tree_results.collect()
    inputs = json.loads((DATA / TREE_RUN / "inputs.json").read_text())
    hashes = {r["file"]: r["sha256"] for r in historical}
    if {name: (v["bytes"], v["sha256"]) for name, v in inputs.items()} != {n: (sizes[n], hashes[n]) for n in sizes}:
        raise ValueError("Trie run input identities differ from the corpus")
    rows = []
    for row in read_rows(f"{TREE_RUN}/rows.tsv"):
        if row["build"] != "tree4":
            continue
        rows.append(dict(rep=row["rep"], file=row["file"], codec="acz3", bytes=str(sizes[row["file"]]),
                         sha256=hashes[row["file"]], packed=row["bytes"],
                         **{k: row[k] for k in ("enc_user", "enc_sys", "enc_status", "dec_user", "dec_sys", "dec_status", "cmp")}))
    return rows


def tree_blocks(sizes):
    """Per-block sizes and restarts of the hashed and the trie PPM.

    Both models are the same; only their storage differs, so a block in
    which neither restarts must give identical code strings.
    """
    folder = DATA / TREE_BLOCKS
    manifest = json.loads((folder / "manifest.json").read_text())
    for name, wanted in manifest["files"].items():
        if hashlib.sha256((folder / name).read_bytes()).hexdigest() != wanted:
            raise ValueError(f"Changed block evidence: {name}")
    rows = read_rows(f"{TREE_BLOCKS}/compare.tsv")
    block_size = 1 << 24
    expected = {(name, i) for name, size in sizes.items() for i in range((size + block_size - 1) // block_size)}
    actual = [(r["file"], int(r["block"])) for r in rows]
    if len(actual) != len(set(actual)) or set(actual) != expected:
        raise ValueError("Missing, duplicate, or unexpected trie blocks")
    for row in rows:
        size = min(block_size, sizes[row["file"]] - int(row["block"]) * block_size)
        if int(row["bytes"]) != size or row["tree4_roundtrip"] != "true":
            raise ValueError(f"Invalid trie block: {row}")
        if row["v9_restarts"] == row["tree4_restarts"] == "0" and row["identical"] != "true":
            raise ValueError(f"Blocks without restarts differ: {row}")
    return dict(
        TreeBlocks=len(rows),
        TreeQuietBlocks=sum(r["v9_restarts"] == r["tree4_restarts"] == "0" for r in rows),
        TreeIdenticalBlocks=sum(r["identical"] == "true" for r in rows),
        TreeHashRestarts=sum(int(r["v9_restarts"]) for r in rows),
        TreeRestarts=sum(int(r["tree4_restarts"]) for r in rows),
    )


def validate(rows, labels=None):
    """Require a complete successful matrix before computing any result.

    A failed or missing attempt must stop publication, not disappear from
    the denominator. Sizes and hashes establish that every configuration
    processed the same inputs; stable output sizes let us count them once.
    """
    labels = LABELS if labels is None else labels
    expected = {(rep, name, codec) for codec in labels for rep in
                (REPETITIONS if labels is HISTORICAL_LABELS else repetitions(codec)) for name in DESCRIPTIONS}
    actual = [(r["rep"], r["file"], r["codec"]) for r in rows]
    if len(actual) != len(set(actual)) or set(actual) != expected:
        raise ValueError("Missing, duplicate, or unexpected benchmark attempts")
    for row in rows:
        if (row["enc_status"], row["dec_status"], row["cmp"]) != ("0", "0", "ok"):
            raise ValueError(f"Failed benchmark attempt: {row}")
        for side, kind in itertools.product(("enc", "dec"), ("user", "sys")):
            value = Decimal(row[f"{side}_{kind}"])
            if not value.is_finite() or value < 0:
                raise ValueError(f"Invalid benchmark time: {row}")

    sizes = {}
    for name in DESCRIPTIONS:
        group = [r for r in rows if r["file"] == name]
        if len({(r["sha256"], r["bytes"]) for r in group}) != 1:
            raise ValueError(f"Input identity differs: {name}")
        sizes[name] = int(group[0]["bytes"])
        for codec in labels:
            if len({r["packed"] for r in group if r["codec"] == codec}) != 1:
                raise ValueError(f"Output size differs: {name}/{codec}")
    if sum(sizes.values()) != 211_938_580:
        raise ValueError(f"Unexpected corpus size: {sum(sizes.values())}")
    return sizes


def corpus_time(rows, side):
    """Median of corpus totals, not a sum of per-file medians."""
    totals = [
        sum(Decimal(r[f"{side}_user"]) + Decimal(r[f"{side}_sys"])
            for r in rows if r["rep"] == repetition)
        for repetition in sorted({r["rep"] for r in rows})
    ]
    return statistics.median(totals)


def validate_ppmd_v10(rows, restarts, historical, sizes):
    """Check explicit repetitions, statuses, identities, and block accounting."""
    codecs = ("acz3", "ppmd-o6-256m")
    expected = set(itertools.product(REPETITIONS, sizes, codecs))
    actual = [(r["rep"], r["file"], r["codec"]) for r in rows]
    if len(actual) != len(set(actual)) or set(actual) != expected:
        raise ValueError("Incomplete or duplicate v10 attempts")
    hashes = {r["file"]: r["sha256"] for r in historical}
    normalized = []
    for row in rows:
        if (row["enc_status"], row["dec_status"], row["cmp"]) != ("0", "0", "ok"):
            raise ValueError(f"Failed v10 attempt: {row}")
        if row["input_sha256"] != hashes[row["file"]] or not re.fullmatch(r"[0-9a-f]{64}", row["output_sha256"]):
            raise ValueError(f"Invalid v10 input/output identity: {row}")
        normalized.append(dict(row, packed=row["bytes"], bytes=str(sizes[row["file"]]), sha256=row["input_sha256"]))
    for name, codec in itertools.product(sizes, codecs):
        group = [r for r in rows if r["file"] == name and r["codec"] == codec]
        if len({(r["bytes"], r["output_sha256"]) for r in group}) != 1:
            raise ValueError(f"Unstable v10 output: {name}/{codec}")
    # IDs were checked before sorting; the older helper validates numeric
    # timings, stable sizes, and restart payloads against the selected build.
    ordered = sorted(rows, key=lambda r: (r["file"], r["rep"], codecs.index(r["codec"])))
    validate_ppm_diagnostic(ordered, restarts, normalized, sizes)
    return normalized


def write_ppm_metrics(rows, sizes, results):
    """Macros for the level-3 text: the trie against CM, Zstandard and
    7-Zip's PPMd, and its block restarts against the hashed model's."""
    ppm, cm, zstd, ref = (results[k] for k in ("acz3", "acz4", "zstd19", "ppmd7z"))
    macros = {
        "PpmBytes": f"{ppm['bytes']:,}", "PpmMB": f"{ppm['bytes'] / 1e6:.2f}",
        "PpmFraction": f"{100 * ppm['fraction']:.2f}",
        "PpmEncode": f"{Decimal(ppm['enc']):.2f}", "PpmDecode": f"{Decimal(ppm['dec']):.2f}",
        "PpmLargerCM": f"{100 * (ppm['bytes'] / cm['bytes'] - 1):.2f}",
        "PpmSmallerZstd": f"{100 * (1 - ppm['bytes'] / zstd['bytes']):.2f}",
        "PpmCMEncodeRatio": f"{Decimal(cm['enc']) / Decimal(ppm['enc']):.2f}",
        "PpmCMDecodeRatio": f"{Decimal(cm['dec']) / Decimal(ppm['dec']):.2f}",
        "PpmRefBytes": f"{ref['bytes']:,}", "PpmRefEncode": ref["enc"], "PpmRefDecode": ref["dec"],
        "PpmRefSmaller": f"{100 * (1 - ref['bytes'] / ppm['bytes']):.2f}",
        "PpmRefEncodeRatio": f"{Decimal(ppm['enc']) / Decimal(ref['enc']):.2f}",
        "PpmRefDecodeRatio": f"{Decimal(ppm['dec']) / Decimal(ref['dec']):.2f}",
    }
    for side in ("enc", "dec"):
        values = [sum(Decimal(r[f"{side}_user"]) + Decimal(r[f"{side}_sys"])
                      for r in rows if r["codec"] == "acz3" and r["rep"] == rep) for rep in repetitions("acz3")]
        macros["Ppm" + ("Encode" if side == "enc" else "Decode") + "Range"] = f"{min(values):.2f}--{max(values):.2f}"
    xz, o2, dd = (results[k] for k in ("xz9", "acz2", "acz4dedup"))
    raw = sum(sizes.values())
    macros.update({
        "CmBytes": f"{cm['bytes']:,}", "CmMB": f"{cm['bytes'] / 1e6:.2f}", "CmFraction": f"{100 * cm['fraction']:.2f}",
        "InputMB": f"{raw / 1e6:.2f}",
        "CmEncode": f"{Decimal(cm['enc']):.2f}", "CmDecode": f"{Decimal(cm['dec']):.2f}",
        "CmSmallerXz": f"{100 * (1 - cm['bytes'] / xz['bytes']):.2f}",
        "CmXzEncodeRatio": f"{Decimal(cm['enc']) / Decimal(xz['enc']):.2f}",
        "CmXzDecodeRatio": f"{Decimal(cm['dec']) / Decimal(xz['dec']):.0f}",
        "OTwoEncode": f"{Decimal(o2['enc']):.2f}", "OTwoFraction": f"{100 * o2['fraction']:.2f}",
        "DedupBytes": f"{dd['bytes']:,}", "DedupLarger": f"{100 * (dd['bytes'] / cm['bytes'] - 1):.3f}",
        "DedupEncode": f"{Decimal(dd['enc']):.2f}", "DedupDecode": f"{Decimal(dd['dec']):.2f}",
        "DedupEncodeLess": f"{100 * (1 - Decimal(dd['enc']) / Decimal(cm['enc'])):.2f}",
        "DedupDecodeLess": f"{100 * (1 - Decimal(dd['dec']) / Decimal(cm['dec'])):.2f}",
        "PpmPortLarger": f"{100 * (ppm['bytes'] / 49_224_897 - 1):.2f}",
    })
    macros.update({k: str(v) for k, v in tree_blocks(sizes).items()})
    macros.update(write_storage_table())
    (BUILD / "metrics.tex").write_text("\n".join(rf"\newcommand{{\{k}}}{{{v}}}" for k, v in macros.items()) + "\n")


def write_storage_table():
    """The balanced comparison of one PPM model with its contexts in a hash
    table and in the trie; macros for the text."""
    import build_tree_results
    m = build_tree_results.collect()
    hashed, trie = m["v9"], m["tree4"]
    mib = lambda kib: f"{kib / 1024:.0f}"
    rows = [
        ["Encoding CPU (s)", f"{hashed['enc_cpu']:.2f}", f"{trie['enc_cpu']:.2f}"],
        ["Decoding CPU (s)", f"{hashed['dec_cpu']:.2f}", f"{trie['dec_cpu']:.2f}"],
        ["Encoding elapsed (s)", f"{hashed['enc_elapsed']:.2f}", f"{trie['enc_elapsed']:.2f}"],
        ["Decoding elapsed (s)", f"{hashed['dec_elapsed']:.2f}", f"{trie['dec_elapsed']:.2f}"],
        ["Largest peak RSS, encoding (MiB)", mib(hashed["enc_rss_kib"]), mib(trie["enc_rss_kib"])],
        ["Largest peak RSS, decoding (MiB)", mib(hashed["dec_rss_kib"]), mib(trie["dec_rss_kib"])],
        ["Compressed bytes", f"{hashed['bytes']:,}", f"{trie['bytes']:,}"],
    ]
    write_table("storage.tex", rows)
    rss = lambda b: max(b["enc_rss_kib"], b["dec_rss_kib"])
    return {
        "TrieEncodeReduction": f"{100 * (1 - trie['enc_cpu'] / hashed['enc_cpu']):.1f}",
        "TrieDecodeReduction": f"{100 * (1 - trie['dec_cpu'] / hashed['dec_cpu']):.1f}",
        "TrieRssHash": mib(rss(hashed)), "TrieRss": mib(rss(trie)),
        "TrieRssReduction": f"{100 * (1 - rss(trie) / rss(hashed)):.0f}",
        "TrieBytesSaved": f"{hashed['bytes'] - trie['bytes']:,}",
        "HashEncode": f"{hashed['enc_cpu']:.2f}", "HashDecode": f"{hashed['dec_cpu']:.2f}",
    }


def write_dedup_ablation(sizes):
    rows = read_rows("dedup-size-pilot.tsv")
    totals, table = validate_dedup_ablation(rows, sizes)
    # The pilot's level-3 column measured the PPMd-H port, which acz no
    # longer contains; the table reports CM and the commands.
    write_table("dedup-ablation.tex", [[threshold, cm, commands] for threshold, _, cm, commands in table])


def validate_dedup_ablation(rows, sizes):
    models, thresholds, block_size = ("ppmd", "cm"), (0, 32, 64, 128, 256), 1 << 24
    expected = {(name, model, threshold, block) for name, size in sizes.items()
                for model in models for threshold in thresholds for block in range((size + block_size - 1) // block_size)}
    keys = [(Path(r["path"]).name, r["model"], int(r["threshold"]), int(r["block"])) for r in rows]
    if len(keys) != len(set(keys)) or set(keys) != expected:
        raise ValueError("Incomplete or duplicate dedup ablation")
    original_hashes = {r["file"]: r["sha256"] for r in historical_rows()}
    inputs = json.loads((DATA / "dedup-size-pilot-inputs.json").read_text())
    if {Path(r["path"]).name: r["sha256"] for r in inputs} != original_hashes:
        raise ValueError("Dedup ablation input identities differ")
    def varint_len(n):
        return max(1, (n.bit_length() + 6) // 7)
    for row, key in zip(rows, keys):
        name, _, threshold, block = key
        raw, residual, body, commands, copied, copies = (int(row[k]) for k in ("raw", "residual", "body", "commands", "copied", "copies"))
        if min(body, commands, copied, copies) < 0:
            raise ValueError("Negative ablation accounting")
        if raw != min(block_size, sizes[name] - block * block_size) or not 0 <= residual <= raw or copied + residual != raw or row["roundtrip"] != "ok":
            raise ValueError("Invalid ablation block or round trip")
        if not ((row["coded"] == "1" and 0 < body < residual) or (row["coded"] == "0" and body == residual)):
            raise ValueError("Invalid ablation stored-block choice")
        if threshold:
            overhead = varint_len(commands) + commands + varint_len(residual)
            if copies and not threshold <= int(row["shortest"]) <= int(row["longest"]) <= raw:
                raise ValueError("Copy below ablation threshold")
            if not copies * threshold <= copied <= copies * raw:
                raise ValueError("Invalid ablation copy total")
        else:
            overhead = 0
            if commands or copied or copies:
                raise ValueError("Copies present with dedup disabled")
        if int(row["framed_block"]) != 17 + overhead + body:
            raise ValueError("Invalid ablation framing total")
    by_key = dict(zip(keys, rows))
    for name, _, threshold, block in keys:
        ppmd, cm = (by_key[name, model, threshold, block] for model in models)
        for field in ("raw", "commands", "residual", "copies", "copied", "shortest", "longest"):
            if ppmd[field] != cm[field]:
                raise ValueError("Repeat removal differs between downstream models")
    totals, table = {}, []
    for threshold in thresholds:
        cells = ["Off" if threshold == 0 else str(threshold)]
        for model in models:
            group = [r for r in rows if r["model"] == model and int(r["threshold"]) == threshold]
            total = 33 * len(sizes) + sum(int(r["framed_block"]) for r in group)
            totals[model, threshold] = total
            cells.append(f"{total:,}")
        command = sum(int(r["commands"]) for r in rows if r["model"] == "ppmd" and int(r["threshold"]) == threshold)
        cells.append(f"{command:,}")
        table.append(cells)
    if (totals["cm", 0], totals["cm", 256], totals["ppmd", 0]) != (42_764_432, 42_796_316, 49_224_897):
        raise ValueError("Ablation controls differ from full CLI measurements")
    return totals, table


def validate_ppm_diagnostic(rows, restarts, primary, sizes):
    """Check the separate PPM pass against itself and the original evidence.

    The original diagnostic omitted repetition IDs. Its retained command
    script emits three consecutive acz/PPMd pairs per file; require that
    exact layout before assigning repetition numbers. These times never
    replace the original experiment's rows.
    """
    codecs = ("acz3", "ppmd-o6-256m")
    if len(rows) != len(sizes) * 6:
        raise ValueError("Incomplete PPM diagnostic")
    baseline = {r["file"]: int(r["packed"]) for r in primary if r["codec"] == "acz3"}
    normalized, seen = [], set()
    for offset in range(0, len(rows), 6):
        group = rows[offset:offset + 6]
        name = group[0]["file"]
        if name not in sizes or name in seen:
            raise ValueError(f"Unexpected or duplicate diagnostic file: {name}")
        seen.add(name)
        for i, row in enumerate(group):
            if row["file"] != name or row["codec"] != codecs[i % 2] or row["cmp"] != "ok":
                raise ValueError(f"Failed or misordered PPM diagnostic: {row}")
            for side, kind in itertools.product(("enc", "dec"), ("user", "sys")):
                value = Decimal(row[f"{side}_{kind}"])
                if not value.is_finite() or value < 0:
                    raise ValueError(f"Invalid diagnostic time: {row}")
            if int(row["bytes"]) <= 0:
                raise ValueError(f"Invalid diagnostic size: {row}")
            normalized.append(dict(row, rep=str(i // 2 + 1), packed=row["bytes"]))
        for codec in codecs:
            values = {int(r["bytes"]) for r in group if r["codec"] == codec}
            if len(values) != 1 or (codec == "acz3" and values != {baseline[name]}):
                raise ValueError(f"Diagnostic size mismatch: {name}/{codec}")

    block_size = 1 << 24
    expected = {(name, i) for name, size in sizes.items()
                for i in range((size + block_size - 1) // block_size)}
    actual = [(r["file"], int(r["block"])) for r in restarts]
    if len(actual) != len(set(actual)) or set(actual) != expected:
        raise ValueError("Missing, duplicate, or unexpected restart blocks")
    for row in restarts:
        size = min(block_size, sizes[row["file"]] - int(row["block"]) * block_size)
        modeled, payload = int(row["modeled_bytes"]), int(row["payload_bytes"])
        if int(row["bytes"]) != size or not 0 <= modeled <= size or int(row["restarts"]) < 0:
            raise ValueError(f"Invalid restart accounting: {row}")
        if row["storage"] == "compressed":
            valid = modeled == size and 0 < payload < size
        elif row["storage"] == "stored":
            valid = payload == size
        else:
            valid = False
        if not valid:
            raise ValueError(f"Invalid block storage decision: {row}")
    for name in sizes:
        group = [r for r in restarts if r["file"] == name]
        framed = sum(int(r["payload_bytes"]) for r in group) + 33 + 17 * len(group)
        if framed != baseline[name]:
            raise ValueError(f"Diagnostic framing does not reproduce original size: {name}")
    return normalized


def write_table(name, rows):
    text = "\n".join(" & ".join(row) + r" \\" for row in rows) + "\n"
    (BUILD / name).write_text(text, encoding="utf-8")


def write_tables(rows, sizes):
    """Generate presentation tables and machine-readable totals together."""
    raw = sum(sizes.values())
    results = {}
    for codec in LABELS:
        group = [r for r in rows if r["codec"] == codec]
        packed = sum(int(r["packed"]) for r in group if r["rep"] == "1")
        encode, decode = corpus_time(group, "enc"), corpus_time(group, "dec")
        results[codec] = dict(bytes=packed, fraction=packed / raw, enc=str(encode), dec=str(decode))
    write_table("totals.tex", [
        [LABELS[c], f"{r['bytes']:,}", f"{100 * r['fraction']:.2f}", f"{Decimal(r['enc']):.2f}", f"{Decimal(r['dec']):.2f}"]
        for c, r in sorted(results.items(), key=lambda item: item[1]["bytes"])])

    packed_sizes = {(r["file"], r["codec"]): int(r["packed"]) for r in rows}
    per_file = []
    for name, description in DESCRIPTIONS.items():
        cells = [name, description, f"{sizes[name] / 1e6:.2f}"]
        cells += [f"{100 * packed_sizes[name, codec] / sizes[name]:.2f}"
                  for codec in ("acz2", "acz3", "acz4", "xz9", "zstd19")]
        per_file.append(cells)
    write_table("files.tex", per_file)
    (BUILD / "results.json").write_text(json.dumps(results, indent=2) + "\n", encoding="utf-8")
    manifest = {
        str(path.relative_to(ROOT)): hashlib.sha256(path.read_bytes()).hexdigest()
        for path in sorted(DATA.rglob("*")) if path.is_file()
    }
    (BUILD / "evidence-sha256.json").write_text(json.dumps(manifest, indent=2) + "\n", encoding="utf-8")
    return results


def write_plots(rows, sizes, results):
    """Plot compressed fractions and measured CPU time.

    The Kiviat axes share one 0--100% scale, with no per-axis rescaling.
    The time plots use logarithmic x axes because decoding times span
    three orders of magnitude. The five curves use consistent styles.
    """
    styles = {
        "acz4": ("CM", "color=cmColor,mark=*,mark options={solid,draw=cmColor,fill=cmColor},solid"),
        "acz3": ("PPM", "color=ppmColor,mark=triangle*,mark options={solid,draw=ppmColor,fill=ppmColor},dashed"),
        "acz2": ("Order 2", "color=oTwoColor,mark=diamond*,mark options={solid,draw=oTwoColor,fill=oTwoColor},dotted"),
        "xz9": ("XZ", "color=xzColor,mark=x,mark options={solid,draw=xzColor,fill=xzColor},solid"),
        "zstd19": ("Zstandard", "color=zstdColor,mark=square*,mark options={solid,draw=zstdColor,fill=zstdColor},dashdotted"),
    }
    packed = {(r["file"], r["codec"]): int(r["packed"]) for r in rows}
    angles = {name: (90 - 30 * i) % 360 for i, name in enumerate(DESCRIPTIONS)}
    labels = {angle: name for name, angle in angles.items()}
    ticks = ",".join(str(a) for a in sorted(labels))
    names = ",".join(labels[a] for a in sorted(labels))
    radar = [r"\begin{tikzpicture}", r"\begin{polaraxis}[",
             r"width=10.7cm,height=10.7cm,ymin=0,ymax=100,ytick={25,50,75,100},",
             f"xtick={{{ticks}}},xticklabels={{{names}}},",
             r"grid=both,grid style={gray!25},axis line style={gray!50},",
             r"xticklabel style={font=\small},yticklabel style={font=\scriptsize,anchor=north east,xshift=-3pt,yshift=-5pt,fill=white,inner sep=1pt},",
             r"legend style={at={(0.5,-0.14)},anchor=north,draw=none,font=\small},legend columns=5]"]
    for codec, (label, style) in styles.items():
        values = [(angles[name], 100 * packed[name, codec] / sizes[name]) for name in DESCRIPTIONS]
        coords = " ".join(f"({a},{v:.6f})" for a, v in values + values[:1])
        radar += [rf"\addplot+[{style},line width=0.85pt,mark size=1.7pt] coordinates {{{coords}}};",
                  rf"\addlegendentry{{{label}}}"]
    radar += [r"\end{polaraxis}", r"\end{tikzpicture}"]
    (BUILD / "kiviat.tex").write_text("\n".join(radar) + "\n", encoding="utf-8")

    plots = []
    for side, title in (("enc", "Encoding"), ("dec", "Decoding")):
        plots += [r"\begin{tikzpicture}", r"\begin{semilogxaxis}[",
                  r"width=0.48\linewidth,height=5.5cm,ymin=18,ymax=35,",
                  rf"title={{{title}}},xlabel={{CPU seconds (log scale)}},ylabel={{Compressed size (\% of input)}},",
                  r"grid=both,grid style={gray!20},minor grid style={gray!10},",
                  r"tick label style={font=\scriptsize},label style={font=\small},title style={font=\small},"]
        if side == "enc":
            plots += [r"xmin=5,xmax=200,xtick={5,10,20,50,100,200},xticklabels={5,10,20,50,100,200},",
                      r"legend to name=costlegend,legend columns=5,legend style={draw=none,font=\small,column sep=5pt},]"]
        else:
            plots += [r"xmin=0.1,xmax=300,xtick={0.1,1,10,100},xticklabels={0.1,1,10,100},]"]
        for codec, (label, style) in styles.items():
            point = results[codec]
            plots += [rf"\addplot+[{style},only marks,mark size=2.6pt] coordinates {{({point[side]},{100 * point['fraction']:.6f})}};"]
            if side == "enc":
                plots += [rf"\addlegendentry{{{label}}}"]
        plots += [r"\end{semilogxaxis}", r"\end{tikzpicture}"]
        if side == "enc":
            plots += [r"\hfill"]
    plots += [r"\par\medskip\ref{costlegend}"]
    (BUILD / "costs.tex").write_text("\n".join(plots) + "\n", encoding="utf-8")


def typeset():
    """Resolve references, reject layout errors, then publish the PDF."""
    tex = shutil.which("pdflatex")
    if not tex:
        raise SystemExit("pdflatex is required (TeX Live or MacTeX).")
    for _ in range(2):
        run = subprocess.run(
            [tex, "-interaction=nonstopmode", "-halt-on-error", "-output-directory=build", "report.tex"],
            cwd=ROOT, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True,
        )
        if run.returncode:
            print(run.stdout[-9000:])
            raise SystemExit(run.returncode)
    log = (BUILD / "report.log").read_text()
    if any(warning in log for warning in ("undefined references", "Citation `", "Overfull")):
        raise SystemExit("Typesetting warning requires review; see report/build/report.log")
    OUTPUT.parent.mkdir(parents=True, exist_ok=True)
    shutil.copyfile(BUILD / "report.pdf", OUTPUT)
    shutil.copyfile(BUILD / "report.pdf", ROOT / "report.pdf")


def validate_coded_commands(rows, historical):
    """Keep the v11 size experiment complete and tied to the original inputs."""
    expected = set(itertools.product(DESCRIPTIONS, (3, 4)))
    actual = [(r["file"], r["model"]) for r in rows]
    if len(actual) != len(set(actual)) or set(actual) != expected:
        raise ValueError("Missing, duplicate, or unexpected v11 containers")
    inputs = {r["file"]: (int(r["bytes"]), r["sha256"]) for r in historical}
    for row in rows:
        if (row["raw_bytes"], row["raw_sha256"]) != inputs[row["file"]]:
            raise ValueError("v11 input identity differs")
        if row["roundtrip"] is not True or not isinstance(row["packed_bytes"], int) or row["packed_bytes"] <= 0:
            raise ValueError("Failed or invalid v11 container")
        if not re.fullmatch(r"[0-9a-f]{64}", row["packed_sha256"]):
            raise ValueError("Invalid v11 output identity")
    return {model: sum(r["packed_bytes"] for r in rows if r["model"] == model) for model in (3, 4)}


def check_evidence(files):
    """Check (path under data/, SHA-256) pairs. A missing file is an error
    unless it is one of the optional archives; returns those found missing."""
    absent = []
    for name, wanted in files:
        if not isinstance(wanted, str) or not re.fullmatch(r"[0-9a-f]{64}", wanted):
            raise ValueError(f"Invalid evidence identity: {name}")
        path = DATA / name
        if not path.exists():
            if name not in OPTIONAL_ARCHIVES:
                raise ValueError(f"Missing evidence: {name}")
            absent.append(name)
        elif hashlib.sha256(path.read_bytes()).hexdigest() != wanted:
            raise ValueError(f"Changed evidence: {name}")
    return absent


def coded_command_evidence():
    """The v11 experiment's manifest, with its rows and source archive
    checked; and the optional archives that are absent."""
    folder = DATA / "dedup-v11-69315de"
    manifest = json.loads((folder / "manifest.json").read_text())
    absent = check_evidence([("dedup-v11-69315de/rows.json", manifest["rows_sha256"]),
                             ("dedup-v11-69315de/source.tar.gz", manifest["source_archive_sha256"])])
    return manifest, absent


def write_coded_commands(historical, results):
    folder = DATA / "dedup-v11-69315de"
    manifest, absent = coded_command_evidence()
    totals = validate_coded_commands(json.loads((folder / "rows.json").read_text()), historical)
    identity = manifest["identity"]
    identity_path = folder / "identity-rows.json"
    if hashlib.sha256(identity_path.read_bytes()).hexdigest() != identity["rows_sha256"]:
        raise ValueError("Changed v11 identity evidence")
    macros = coded_command_metadata(manifest, json.loads(identity_path.read_text()))
    (BUILD / "coded-command-metrics.tex").write_text(
        "\n".join(rf"\newcommand{{\{key}}}{{{value}}}" for key, value in macros.items()) + "\n")
    # Model 3 of that experiment was the PPMd-H port; only CM is reported.
    off = sum(int(r["packed"]) for r in historical if r["codec"] == "acz4" and r["rep"] == "1")
    write_table("coded-commands.tex", [["CM", f"{off:,}", f"{totals[4]:,}", f"{off - totals[4]:,}"]])
    return absent


def coded_command_metadata(manifest, identity_rows):
    """Derive toolchain and identity coverage from the retained evidence."""
    compiler = re.search(r"^release: (\d+\.\d+\.\d+)$", manifest["compiler"], re.MULTILINE)
    if not compiler or not manifest["compiler"].startswith(f"rustc {compiler[1]} "):
        raise ValueError("Missing or inconsistent v11 compiler identity")
    labels = {
        "empty": "empty", "one": "one-byte", "alphabet": "256-byte alphabet",
        "noise": "pseudorandom", "periodic": "periodic", "text": "text", "cross_block": "multi-block",
    }
    expected = set(itertools.product(range(5), labels, ("64K", "128K")))
    actual = [(r["model"], r["input"], r["block"]) for r in identity_rows]
    if len(actual) != len(set(actual)) or set(actual) != expected or len(actual) != manifest["identity"]["cases"]:
        raise ValueError("Incomplete, duplicate, or unexpected v11 identity cases")
    for row in identity_rows:
        if row["payload_equal"] is not True or row["roundtrip"] is not True or row["v10_bytes"] != row["v11_bytes"]:
            raise ValueError("Failed v11 identity case")
    names = list(labels.values())
    return {
        "CommandCompiler": compiler[1],
        "CommandIdentityCases": len(identity_rows),
        "CommandIdentityInputs": ", ".join(names[:-1]) + ", and " + names[-1],
    }


def main():
    historical = historical_rows()
    sizes = validate(historical, HISTORICAL_LABELS)
    # Retained evidence of earlier level-3 models is still checked, though
    # the report no longer tabulates it.
    validate_ppm_diagnostic(read_rows("ppm-diagnostic.tsv"), read_rows("ppm-restarts.tsv"), historical, sizes)
    rows = selected_rows()
    validate(rows)
    BUILD.mkdir(exist_ok=True)
    results = write_tables(rows, sizes)
    write_plots(rows, sizes, results)
    write_ppm_metrics(rows, sizes, results)
    write_dedup_ablation(sizes)
    absent = write_coded_commands(historical, results)
    import build_current_results
    build_current_results.write_tables(BUILD)
    import build_heldout_results
    build_heldout_results.write_tables(BUILD)
    typeset()
    print(f"Validated 360 historical and {len(rows)} selected attempts (216 of release 0.7.0), the 96-case trie run, 19 trie blocks, "
          f"both 72-row diagnostics, 190 ablation blocks, 24 v11 containers, 108 run5 attempts, "
          f"759 development code sizes, 12 format13 identity records and 11,746 held-out inputs; wrote {OUTPUT}")
    for name in absent:
        print(f"Not present, retained privately (it contains the removed PPMd-H port): data/{name}")


if __name__ == "__main__":
    main()
