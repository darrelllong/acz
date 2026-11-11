"""Reduce the format-14 level-4 rows to one row per input.

Run on knuth from ~/acz-eval:  python3 reduce_heldout_v14.py census OUT.tsv
The run measured acz14 = 549d4ff (release 0.7.0) at -4 on the sample lists
of the earlier runs. Census ran on knuth from the same paths: main-v14, and
main-v14-rest for the files queued on one worker behind the largest file,
which the other cores took once they were free. Backblaze ran
on sekhmet from /styx, a copy of /gilgamesh, so its rows carry the
identifier of the /styx path; backblaze/main-v14-styx-mapping.tsv gives the
original path of each. Every input in the sample list must have exactly one
row.
"""
import csv
import glob
import hashlib
import sys

COLS = ("host path_id src_bytes src_sha256 unpack unpack_status complete content_bytes "
        "content_sha256 codec packed enc_user enc_sys enc_wall enc_status dec_user dec_sys "
        "dec_wall dec_status cmp").split()
RUNS = {"census": ["census/main-v14", "census/main-v14-rest"],
        "backblaze": ["backblaze/main-v14-sekhmet-big", "backblaze/main-v14-sekhmet-little"]}
PLAN = {"census": "census/main.tsv", "backblaze": "backblaze/main-proposed.tsv"}


def path_id(path):
    return hashlib.sha256(path.encode()).hexdigest()[:16]


def main():
    collection, dest = sys.argv[1], sys.argv[2]
    original = {}
    if collection == "backblaze":
        for line in open("backblaze/main-v14-styx-mapping.tsv"):
            styx, source = line.rstrip("\n").split("\t")
            original[path_id(styx)] = path_id(source)
    rows = {}
    for run in RUNS[collection]:
        for name in sorted(glob.glob(run + "/*.tsv")):
            for r in csv.reader(open(name), delimiter="\t"):
                r = dict(zip(COLS, r, strict=True))
                assert r["codec"] == "acz4", r
                key = original.get(r["path_id"], r["path_id"])
                assert key not in rows, ("duplicate", key)
                rows[key] = r
    inputs = [path_id(line.rstrip("\n").split("\t")[2]) for line in open(PLAN[collection])]
    assert len(set(inputs)) == len(inputs) == len(rows), (len(inputs), len(rows))
    with open(dest, "w", newline="") as f:
        w = csv.writer(f, delimiter="\t", lineterminator="\n")
        w.writerow(["path_id", "content_bytes", "content_sha256", "acz4", "status"])
        for pid in inputs:
            r = rows[pid]
            ok = r["cmp"] == "ok"
            w.writerow([pid, r["content_bytes"], r["content_sha256"], r["packed"] if ok else "-", r["cmp"]])


if __name__ == "__main__":
    main()
