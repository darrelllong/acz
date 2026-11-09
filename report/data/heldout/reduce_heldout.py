"""Reduce the held-out measurement rows to one row per input.

Run on knuth from ~/acz-eval:  python3 reduce_heldout.py census OUT.tsv
The original run (acz10 = debc62e) gives acz4, xz9, zstd19 and 0.5.0's
level 3; the v13 run (acz13 = a3c9440) gives the new level 3.  Every input
in the sample list must have exactly one row per codec per run, and both
runs must have measured the same content (SHA-256).
"""
import csv
import glob
import hashlib
import sys

COLS = ("host path_id src_bytes src_sha256 unpack unpack_status complete content_bytes "
        "content_sha256 codec packed enc_user enc_sys enc_wall enc_status dec_user dec_sys "
        "dec_wall dec_status cmp").split()


def rows(run):
    out = {}
    for name in sorted(glob.glob(run + "/*.tsv")):
        for r in csv.reader(open(name), delimiter="\t"):
            r = dict(zip(COLS, r, strict=True))
            key = (r["path_id"], r["codec"])
            assert key not in out, ("duplicate", run, key)
            out[key] = r
    return out


def main():
    collection, dest = sys.argv[1], sys.argv[2]
    plan = {"census": "census/main.tsv", "backblaze": "backblaze/main-proposed.tsv"}[collection]
    inputs = []
    for line in open(plan):
        size, kind, path = line.rstrip("\n").split("\t")
        inputs.append((hashlib.sha256(path.encode()).hexdigest()[:16], kind))
    assert len({i for i, _ in inputs}) == len(inputs)
    old, new = rows(collection + "/main-run"), rows(collection + "/main-v13")
    assert len(old) == 4 * len(inputs) and len(new) == len(inputs), (len(old), len(new))
    with open(dest, "w", newline="") as f:
        w = csv.writer(f, delimiter="\t", lineterminator="\n")
        w.writerow(["path_id", "type", "content_bytes", "content_sha256",
                    "acz4", "xz9", "zstd19", "acz3_0.5.0", "acz3", "status"])
        for pid, kind in inputs:
            got = [old[pid, c] for c in ("acz4", "xz9", "zstd19", "acz3")] + [new[pid, "acz3"]]
            content = {(r["content_bytes"], r["content_sha256"]) for r in got}
            assert len(content) == 1, pid
            (cb, ch), = content
            states = {r["cmp"] for r in got}
            status = "ok" if states == {"ok"} else "/".join(sorted(states))
            packed = [r["packed"] if r["cmp"] == "ok" else "-" for r in got]
            w.writerow([pid, kind, cb, ch, *packed, status])


if __name__ == "__main__":
    main()
