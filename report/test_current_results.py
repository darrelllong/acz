"""Independent negative controls for the new report evidence selection."""
import copy
import json
import unittest
from decimal import Decimal
import build_current_results as current


class CurrentEvidenceTests(unittest.TestCase):
    def setUp(self):
        self.rows, self.inputs, self.metrics = current.run5()
        self.outputs = json.loads((current.RUN5 / "outputs.json").read_text())

    def test_recorded_medians_and_totals(self):
        self.assertEqual(self.metrics["new"]["bytes"], 50_601_960)
        self.assertEqual(self.metrics["new"]["dec_cpu"], Decimal("23.56"))
        self.assertEqual(self.metrics["trie"]["enc_cpu"], Decimal("24.23"))
        self.assertEqual(self.metrics["port"]["bytes"], 49_224_897)

    def test_reject_missing_duplicate_and_reordered_cases(self):
        swapped = copy.deepcopy(self.rows)
        swapped[0], swapped[1] = swapped[1], swapped[0]
        for rows in (self.rows[:-1], self.rows[:-1] + self.rows[:1], swapped):
            with self.assertRaises(ValueError):
                current.validate_run5(rows, self.inputs, self.outputs)

    def test_reject_failed_and_invalid_measurements(self):
        for key, value in (("cmp", "DIFF"), ("enc_status", "1"), ("dec_status", "1"),
                           ("enc_user", "NaN"), ("dec_elapsed", "-1"),
                           ("enc_rss_kib", "0"), ("out_sha256", "0" * 64)):
            rows = copy.deepcopy(self.rows)
            rows[0][key] = value
            with self.subTest(key=key), self.assertRaises(ValueError):
                current.validate_run5(rows, self.inputs, self.outputs)

    def test_median_is_over_corpus_sums(self):
        rows = copy.deepcopy(self.rows)
        # Put different large outliers in distinct files/repetitions. The
        # median of sums differs from both a mean and a sum of file medians.
        chosen = [r for r in rows if r["build"] == "new"]
        for rep, name, extra in (("1", "dickens", 10), ("2", "mozilla", 20)):
            r = next(r for r in chosen if r["rep"] == rep and r["file"] == name)
            r["enc_user"] = str(Decimal(r["enc_user"]) + extra)
        result = current.validate_run5(rows, self.inputs, self.outputs)
        self.assertEqual(result["new"]["enc_cpu"], Decimal("31.64"))

    def test_selection_retains_regressions_and_duplicate_weighting(self):
        rows = current.run21()
        five, ee = rows[current.FIVE], rows[current.FIVE + "+ee1"]
        names = set(five) - {"config", "total", "encode_s", "verify_s"}
        self.assertEqual(sum(int(ee[n]) > int(five[n]) for n in names), 31)
        self.assertEqual(int(five["geo"]), 56683)
        self.assertEqual(int(five["total"]), 27522780)
        inputs = json.loads((current.RUN21 / "inputs.json").read_text())
        self.assertEqual(len({v["actual"] for v in inputs.values()}), 32)

    def test_all_development_stages_and_shared_output_identity(self):
        stages = current.stages()
        self.assertEqual(sum(len(rows) * 33 for rows in stages.values()), 759)
        self.assertEqual(int(stages[11]["bin6+par"]["total"]), 28754487)
        self.assertEqual(int(stages[13]["seef"]["total"]), 27863485)
        self.assertEqual(stages[18][current.FIVE]["total"], stages[21][current.FIVE]["total"])

    def test_reject_changed_size_totals_or_output_identity(self):
        inputs = json.loads((current.RUN21 / "inputs.json").read_text())
        outputs = json.loads((current.RUN21 / "outputs.json").read_text())
        rows = current.read_rows(current.RUN21 / "sizes.tsv")
        rows[0]["total"] = "1"
        with self.assertRaises(ValueError):
            current.validate_run21(rows, inputs, outputs)
        rows = current.read_rows(current.RUN21 / "sizes.tsv")
        outputs[next(iter(outputs))]["actual"] = "0" * 64
        with self.assertRaises(ValueError):
            current.validate_run21(rows, inputs, outputs)


if __name__ == "__main__":
    unittest.main()
