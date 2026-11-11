"""Reject incomplete or inconsistent report evidence before typesetting."""
import copy
import hashlib
import json
import shutil
import tempfile
import unittest
from pathlib import Path

import build_report as report


class PpmEvidenceTests(unittest.TestCase):
    def setUp(self):
        self.primary = report.historical_rows()
        self.sizes = report.validate(self.primary, report.HISTORICAL_LABELS)
        self.rows = report.read_rows("ppm-diagnostic.tsv")
        self.restarts = report.read_rows("ppm-restarts.tsv")

    def validate(self):
        return report.validate_ppm_diagnostic(self.rows, self.restarts, self.primary, self.sizes)

    def test_retained_corpus_totals(self):
        rows = self.validate()
        for codec, packed, expected in (("acz3", 52_458_591, ("54.86", "52.93")),
                                        ("ppmd-o6-256m", 49_029_343, ("15.75", "16.86"))):
            group = [r for r in rows if r["codec"] == codec]
            self.assertEqual(sum(int(r["packed"]) for r in group if r["rep"] == "1"), packed)
            self.assertEqual(tuple(str(report.corpus_time(group, side)) for side in ("enc", "dec")), expected)
        self.assertEqual(len(self.restarts), 19)
        self.assertTrue(all(r["storage"] == "compressed" for r in self.restarts))
        self.assertEqual(sum(int(r["restarts"]) for r in self.restarts), 16)

    def test_missing_or_duplicate_file(self):
        original = copy.deepcopy(self.rows)
        for bad in (original[:-1], original[:6] + original[:6] + original[12:]):
            with self.subTest(length=len(bad)):
                self.rows = bad
                with self.assertRaises(ValueError):
                    self.validate()

    def test_attempt_order_cannot_be_silently_repaired(self):
        self.rows[1], self.rows[2] = self.rows[2], self.rows[1]
        with self.assertRaises(ValueError):
            self.validate()

    def test_failed_round_trip(self):
        self.rows[0]["cmp"] = "DIFF"
        with self.assertRaises(ValueError):
            self.validate()

    def test_invalid_times(self):
        for value in ("NaN", "Infinity", "-0.01"):
            with self.subTest(value=value):
                self.rows[0]["enc_user"] = value
                with self.assertRaises(ValueError):
                    self.validate()

    def test_unstable_compressed_size(self):
        self.rows[1]["bytes"] = str(int(self.rows[1]["bytes"]) + 1)
        with self.assertRaises(ValueError):
            self.validate()

    def test_acz_must_match_original_sizes(self):
        for row in self.rows[:6:2]:
            row["bytes"] = str(int(row["bytes"]) + 1)
        with self.assertRaises(ValueError):
            self.validate()

    def test_missing_or_duplicate_block(self):
        original = copy.deepcopy(self.restarts)
        for bad in (original[:-1], original[:-1] + [original[0]]):
            with self.subTest(blocks=len(bad)):
                self.restarts = bad
                with self.assertRaises(ValueError):
                    self.validate()

    def test_compressed_block_must_model_all_bytes(self):
        self.restarts[0]["modeled_bytes"] = "1"
        with self.assertRaises(ValueError):
            self.validate()

    def test_payload_and_framing_must_match_original(self):
        self.restarts[0]["payload_bytes"] = str(int(self.restarts[0]["payload_bytes"]) + 1)
        with self.assertRaises(ValueError):
            self.validate()


class TreeEvidenceTests(unittest.TestCase):
    """Level 3 is the PPM on the context trie, from its own balanced run."""

    def setUp(self):
        self.historical = report.historical_rows()
        self.sizes = report.validate(self.historical, report.HISTORICAL_LABELS)

    def test_selection_replaces_level_three_and_adds_the_reference(self):
        rows = report.selected_rows()
        # 216 attempts of release 0.7.0, 144 of the other compressors and
        # 36 reference rows.
        self.assertEqual(len(rows), 396)
        report.validate(rows)
        tree = [r for r in rows if r["codec"] == "acz3"]
        self.assertEqual(sum(int(r["packed"]) for r in tree if r["rep"] == "1"), 50_601_960)
        self.assertEqual((str(report.corpus_time(tree, "enc")), str(report.corpus_time(tree, "dec"))), ("20.28", "19.73"))
        cm = [r for r in rows if r["codec"] == "acz4"]
        self.assertEqual(sum(int(r["packed"]) for r in cm if r["rep"] == "1"), 42_443_079)
        self.assertEqual((str(report.corpus_time(cm, "enc")), str(report.corpus_time(cm, "dec"))), ("152.09", "159.56"))
        ref = [r for r in rows if r["codec"] == "ppmd7z"]
        self.assertEqual(sum(int(r["packed"]) for r in ref if r["rep"] == "1"), 49_029_343)

    def test_blocks_without_restarts_are_identical(self):
        counts = report.tree_blocks(self.sizes)
        self.assertEqual(counts, dict(TreeBlocks=19, TreeQuietBlocks=11, TreeIdenticalBlocks=11,
                                      TreeHashRestarts=16, TreeRestarts=14))

    def test_changed_block_evidence_is_rejected(self):
        original = report.read_rows
        def tampered(name):
            rows = original(name)
            if name.endswith("compare.tsv"):
                rows[0]["identical"] = "false"
            return rows
        report.read_rows = tampered
        try:
            # The manifest hash still matches the file, so the check that
            # fails is the identity of a block without restarts.
            with self.assertRaises(ValueError):
                report.tree_blocks(self.sizes)
        finally:
            report.read_rows = original


class PpmdV10EvidenceTests(unittest.TestCase):
    def setUp(self):
        self.historical = report.historical_rows()
        self.sizes = report.validate(self.historical, report.HISTORICAL_LABELS)
        self.rows = report.read_rows(f"{report.PPMD_RUN}/rows.tsv")
        self.restarts = report.read_rows(f"{report.PPMD_RUN}/restarts.tsv")

    def validate(self):
        return report.validate_ppmd_v10(self.rows, self.restarts, self.historical, self.sizes)

    def test_current_matrix_and_sizes(self):
        rows = self.validate()
        current = [r for r in rows if r["codec"] == "acz3"]
        self.assertEqual(sum(int(r["packed"]) for r in current if r["rep"] == "1"), 49_224_897)
        self.assertEqual(sum(int(r["restarts"]) for r in self.restarts), 0)
        self.assertEqual(str(report.corpus_time(current, "enc")), "17.82")
        self.assertEqual(str(report.corpus_time(current, "dec")), "20.33")

    def test_explicit_ids_allow_reordering_but_not_duplicates(self):
        self.rows.reverse()
        self.validate()
        self.rows[0] = self.rows[1]
        with self.assertRaises(ValueError):
            self.validate()

    def test_failed_status_is_rejected_even_with_matching_output(self):
        self.rows[0]["enc_status"] = "1"
        with self.assertRaises(ValueError):
            self.validate()

    def test_input_and_output_hashes_are_checked(self):
        original = copy.deepcopy(self.rows)
        for field, value in (("input_sha256", "0" * 64), ("output_sha256", "invalid"), ("output_sha256", "0" * 64)):
            with self.subTest(field=field, value=value):
                self.rows = copy.deepcopy(original)
                self.rows[0][field] = value
                with self.assertRaises(ValueError):
                    self.validate()

    def test_current_restart_payload_must_match_current_size(self):
        self.restarts[0]["payload_bytes"] = str(int(self.restarts[0]["payload_bytes"]) + 1)
        with self.assertRaises(ValueError):
            self.validate()


class DedupEvidenceTests(unittest.TestCase):
    def setUp(self):
        self.sizes = report.validate(report.historical_rows(), report.HISTORICAL_LABELS)
        self.rows = report.read_rows("dedup-size-pilot.tsv")

    def validate(self):
        return report.validate_dedup_ablation(self.rows, self.sizes)

    def test_complete_pilot_and_threshold_tradeoff(self):
        totals, _ = self.validate()
        self.assertEqual(totals["ppmd", 256] - totals["ppmd", 128], 98_065)
        self.assertEqual(totals["cm", 128] - totals["cm", 256], 218_447)

    def test_missing_or_duplicate_attempt(self):
        original = copy.deepcopy(self.rows)
        for rows in (original[:-1], original[:-1] + [original[0]]):
            self.rows = rows
            with self.assertRaises(ValueError):
                self.validate()

    def test_failed_roundtrip_or_inconsistent_framing(self):
        original = copy.deepcopy(self.rows)
        for key, value in (("roundtrip", "DIFF"), ("framed_block", "1"), ("raw", "1")):
            with self.subTest(key=key):
                self.rows = copy.deepcopy(original)
                self.rows[0][key] = value
                with self.assertRaises(ValueError):
                    self.validate()

    def test_copy_statistics_must_agree_between_models(self):
        row = next(r for r in self.rows if int(r["threshold"]) == 128 and int(r["copies"]) > 2)
        row["copies"] = str(int(row["copies"]) - 1)
        with self.assertRaises(ValueError):
            self.validate()


class CodedCommandEvidenceTests(unittest.TestCase):
    def setUp(self):
        self.historical = report.historical_rows()
        self.rows = json.loads((report.DATA / "dedup-v11-69315de/rows.json").read_text())

    def test_complete_container_totals(self):
        self.assertEqual(report.validate_coded_commands(self.rows, self.historical),
                         {3: 48_550_882, 4: 42_740_598})

    def test_missing_duplicate_or_failed_container(self):
        for rows in (self.rows[:-1], self.rows[:-1] + [self.rows[0]]):
            with self.assertRaises(ValueError):
                report.validate_coded_commands(rows, self.historical)
        for field, value in (("roundtrip", False), ("raw_sha256", "0" * 64),
                             ("packed_sha256", "invalid"), ("packed_bytes", 0)):
            with self.subTest(field=field):
                rows = copy.deepcopy(self.rows)
                rows[0][field] = value
                with self.assertRaises(ValueError):
                    report.validate_coded_commands(rows, self.historical)

    def test_recorded_compiler_and_complete_identity_coverage(self):
        folder = report.DATA / "dedup-v11-69315de"
        manifest = json.loads((folder / "manifest.json").read_text())
        rows = json.loads((folder / "identity-rows.json").read_text())
        metadata = report.coded_command_metadata(manifest, rows)
        self.assertEqual(metadata["CommandCompiler"], "1.93.1")
        self.assertEqual(metadata["CommandIdentityCases"], 70)
        self.assertIn("256-byte alphabet", metadata["CommandIdentityInputs"])
        for bad_rows in (rows[:-1], rows[:-1] + [rows[0]]):
            with self.assertRaises(ValueError):
                report.coded_command_metadata(manifest, bad_rows)
        rows[0]["payload_equal"] = False
        with self.assertRaises(ValueError):
            report.coded_command_metadata(manifest, rows)


class OptionalArchiveTests(unittest.TestCase):
    """The port-containing source archive may be absent, never altered."""

    def setUp(self):
        self.original = report.DATA
        self.tmp = tempfile.TemporaryDirectory()
        folder = Path(self.tmp.name) / "dedup-v11-69315de"
        folder.mkdir()
        for name in ("manifest.json", "rows.json"):
            shutil.copyfile(self.original / "dedup-v11-69315de" / name, folder / name)
        self.folder = folder
        report.DATA = Path(self.tmp.name)

    def tearDown(self):
        report.DATA = self.original
        self.tmp.cleanup()

    def test_absent_archive_is_reported(self):
        _, absent = report.coded_command_evidence()
        self.assertEqual(absent, ["dedup-v11-69315de/source.tar.gz"])

    def test_absent_archive_still_requires_valid_identity(self):
        path = self.folder / "manifest.json"
        manifest = json.loads(path.read_text())
        for invalid in (None, 42, "", "invalid", "a" * 63, "g" * 64):
            with self.subTest(identity=invalid):
                manifest["source_archive_sha256"] = invalid
                path.write_text(json.dumps(manifest))
                with self.assertRaises(ValueError):
                    report.coded_command_evidence()

    def test_present_archive_must_match(self):
        # A stand-in archive and its hash, so that the test does not need the
        # privately retained one.
        archive = b"stand-in source archive"
        manifest = json.loads((self.folder / "manifest.json").read_text())
        manifest["source_archive_sha256"] = hashlib.sha256(archive).hexdigest()
        (self.folder / "manifest.json").write_text(json.dumps(manifest))
        (self.folder / "source.tar.gz").write_bytes(archive)
        self.assertEqual(report.coded_command_evidence()[1], [])
        (self.folder / "source.tar.gz").write_bytes(b"not the archive")
        with self.assertRaises(ValueError):
            report.coded_command_evidence()

    def test_rows_are_required(self):
        (self.folder / "rows.json").unlink()
        with self.assertRaises(ValueError):
            report.coded_command_evidence()

    def test_only_named_archives_are_optional(self):
        with self.assertRaises(ValueError):
            report.check_evidence([("dedup-v11-69315de/v10-source.tar.gz", "0" * 64)])


if __name__ == "__main__":
    unittest.main()
