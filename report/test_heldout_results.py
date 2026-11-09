"""Negative controls for the held-out size evidence."""
import copy
import json
import shutil
import tempfile
import unittest
from pathlib import Path

import build_current_results
import build_heldout_results as heldout


class HeldoutEvidenceTests(unittest.TestCase):
    def setUp(self):
        self.rows = {c: heldout.read_rows(c) for c in heldout.COLLECTIONS}

    def test_recorded_totals(self):
        n, content, packed = heldout.totals("census")
        self.assertEqual((n, content), (11_058, 269_147_642_879))
        n, content, packed = heldout.totals("backblaze")
        self.assertEqual((n, content), (688, 248_284_790_101))
        build = Path(tempfile.mkdtemp())
        self.addCleanup(shutil.rmtree, build)
        macros = heldout.write_tables(build)
        self.assertEqual((macros["CensusCM"], macros["CensusXz"], macros["CensusPpm"]),
                         ("9.89", "10.49", "13.60"))
        self.assertEqual((macros["BackblazeZstd"], macros["BackblazePort"], macros["BackblazePpm"]),
                         ("9.13", "10.10", "10.61"))

    def test_reject_missing_duplicate_and_failed_inputs(self):
        rows = self.rows["backblaze"]
        failed = copy.deepcopy(rows)
        failed[0]["status"] = "DIFF"
        bad_size = copy.deepcopy(rows)
        bad_size[0]["xz9"] = "-"
        bad_hash = copy.deepcopy(rows)
        bad_hash[0]["content_sha256"] = "0" * 63
        for case in (rows[:-1], rows + rows[:1], failed, bad_size, bad_hash):
            with self.assertRaises(ValueError):
                heldout.validate(case, 688, 0)

    def test_empty_input_is_counted_not_measured(self):
        rows = self.rows["census"]
        self.assertEqual(sum(r["status"] == "EMPTY" for r in rows), 1)
        with self.assertRaises(ValueError):
            heldout.validate([r for r in rows if r["status"] != "EMPTY"], 11_058, 1)

    def test_reject_changed_evidence(self):
        folder = Path(tempfile.mkdtemp())
        self.addCleanup(shutil.rmtree, folder)
        for p in heldout.HELDOUT.iterdir():
            shutil.copyfile(p, folder / p.name)
        build_current_results.evidence(folder)
        path = folder / "backblaze-files.tsv"
        path.write_bytes(path.read_bytes().replace(b"\tok\n", b"\tok \n", 1))
        with self.assertRaises(ValueError):
            build_current_results.evidence(folder)
        manifest = json.loads((folder / "manifest.json").read_text())
        self.assertEqual(set(manifest["files"]), {p.name for p in heldout.HELDOUT.iterdir()} - {"manifest.json"})


if __name__ == "__main__":
    unittest.main()
