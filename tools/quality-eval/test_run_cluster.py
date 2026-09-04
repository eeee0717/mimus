import argparse
import importlib.util
import json
import tempfile
import unittest
from pathlib import Path
from unittest import mock


MODULE_PATH = Path(__file__).with_name("run_cluster.py")
SPEC = importlib.util.spec_from_file_location("run_cluster", MODULE_PATH)
assert SPEC is not None and SPEC.loader is not None
CLUSTER = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(CLUSTER)


class RunClusterTest(unittest.TestCase):
    def test_check_pdf_persists_qpdf_evidence(self) -> None:
        with tempfile.TemporaryDirectory() as temp_name:
            evidence = Path(temp_name) / "qpdf-check.txt"
            completed = CLUSTER.subprocess.CompletedProcess(
                args=["qpdf"], returncode=0, stdout=b"checking\n", stderr=b"passed\n"
            )
            with mock.patch.object(CLUSTER.subprocess, "run", return_value=completed) as run:
                CLUSTER.check_pdf(Path("output.pdf"), evidence)

            run.assert_called_once_with(
                ["qpdf", "--check", "output.pdf"], capture_output=True, check=False
            )
            self.assertEqual(evidence.read_bytes(), b"checking\npassed\n")

    def test_check_pdf_rejects_invalid_output(self) -> None:
        with tempfile.TemporaryDirectory() as temp_name:
            evidence = Path(temp_name) / "qpdf-check.txt"
            completed = CLUSTER.subprocess.CompletedProcess(
                args=["qpdf"], returncode=2, stdout=b"", stderr=b"invalid\n"
            )
            with mock.patch.object(CLUSTER.subprocess, "run", return_value=completed):
                with self.assertRaisesRegex(RuntimeError, "qpdf --check failed"):
                    CLUSTER.check_pdf(Path("output.pdf"), evidence)
            self.assertEqual(evidence.read_bytes(), b"invalid\n")

    def test_font_attribution_reads_only_translated_publication_glyphs(self) -> None:
        with tempfile.TemporaryDirectory() as temp_name:
            path = Path(temp_name) / "write.json"
            path.write_text(
                json.dumps(
                    {
                        "pages": [],
                        "publication_ink": [
                            {
                                "components": [
                                    {
                                        "kind": "translated_text",
                                        "glyphs": [
                                            {"unicode": "中", "font_slot": "cjk_regular"},
                                            {"unicode": "A", "font_slot": "latin_regular"},
                                            {"unicode": "A", "font_slot": "latin_regular"},
                                        ],
                                    },
                                    {
                                        "kind": "source_text_replay",
                                        "glyphs": [{"unicode": "x"}],
                                    },
                                ]
                            }
                        ]
                    }
                ),
                encoding="utf-8",
            )
            self.assertEqual(
                CLUSTER.font_attribution(path),
                {
                    "page_count": 0,
                    "glyphs_by_slot": {"cjk_regular": 1, "latin_regular": 2},
                    "unique_scalars_by_slot": {
                        "cjk_regular": ["中"],
                        "latin_regular": ["A"],
                    },
                    "scalar_counts_by_slot": {
                        "cjk_regular": {"中": 1},
                        "latin_regular": {"A": 2},
                    },
                    "missing_font_slot": 0,
                },
            )

    def test_hash_only_command_omits_debug_artifacts(self) -> None:
        args = argparse.Namespace(
            mimus=Path("mimus"),
            endpoint="http://127.0.0.1:1/v1",
            model="conserving-fake",
            font=Path("font.ttf"),
            font_bold=None,
            font_latin=Path("latin.ttf"),
            font_latin_bold=None,
            layout_model=Path("layout.onnx"),
        )
        command = CLUSTER.translation_command(args, Path("input.pdf"), Path("output.pdf"))
        self.assertNotIn("--debug", command)

    def test_manifest_font_run_omits_all_custom_font_flags(self) -> None:
        args = argparse.Namespace(
            mimus=Path("mimus"),
            endpoint="http://127.0.0.1:1/v1",
            model="conserving-fake",
            font=None,
            font_bold=None,
            font_latin=None,
            font_latin_bold=None,
            layout_model=Path("layout.onnx"),
        )
        command = CLUSTER.translation_command(args, Path("input.pdf"), Path("output.pdf"))
        for flag in ("--font", "--font-bold", "--font-latin", "--font-latin-bold"):
            self.assertNotIn(flag, command)

    def test_resume_reuses_internal_failure_without_rerunning(self) -> None:
        with tempfile.TemporaryDirectory() as temp_name:
            root = Path(temp_name)
            corpus = root / "17-failed"
            corpus.mkdir()
            output = root / "output"
            output.mkdir()
            expected = {"paper": "17-failed", "status": "internal-error"}
            (output / "17-failed.failure.json").write_text(json.dumps(expected))
            report = CLUSTER.resumed_report(argparse.Namespace(output_dir=output), corpus)
            self.assertEqual(report, expected)

    def test_aggregate_keeps_internal_failures_and_producer_distribution(self) -> None:
        rows = [
            {
                "paper": "published",
                "producer": "Word",
                "published": True,
                "internal_errors": 0,
                "typed_degraded_paragraphs": 4,
                "ink_violations": 0,
            },
            {
                "paper": "failed",
                "producer": "Word",
                "published": False,
                "internal_errors": 1,
                "typed_degraded_paragraphs": None,
                "ink_violations": None,
            },
        ]
        summary = CLUSTER.aggregate(rows, [])
        self.assertEqual(summary["cluster"]["publication_rate"], 0.5)
        self.assertEqual(summary["cluster"]["internal_rate"], 0.5)
        self.assertEqual(summary["cluster"]["by_producer"]["Word"]["typed_degradation_worst"], 4)

    def test_markdown_renders_not_applicable_values(self) -> None:
        summary = {
            "papers": [{
                "paper": "failed",
                "producer": "Word",
                "v1_total_score": 90.0,
                "v2_total_score": None,
                "published": False,
                "internal_reason": "output_mismatch",
                "typed_degraded_paragraphs": None,
                "conservation_rate": None,
                "formula_proxy_violations": None,
                "continuity_violations": None,
                "inline_hole_count": None,
                "ink_violations": None,
                "title_author_failures": None,
                "translation_calls_per_eligible_paragraph": None,
                "retry_rate": None,
                "echo_rate": None,
                "cache_hit_rate": None,
            }],
            "cluster": {
                "publication_rate": 0.0,
                "internal_rate": 1.0,
                "typed_degradation_median": None,
                "typed_degradation_worst": None,
                "ink_violations": 0,
            },
        }
        rendered = CLUSTER.markdown(summary)
        self.assertIn("Internal/6: output_mismatch", rendered)
        self.assertIn("N/A", rendered)
        table_rows = [line for line in rendered.splitlines() if line.startswith("|")]
        self.assertEqual(len(table_rows), 3)
        self.assertTrue(all(line.count("|") == 17 for line in table_rows))


if __name__ == "__main__":
    unittest.main()
