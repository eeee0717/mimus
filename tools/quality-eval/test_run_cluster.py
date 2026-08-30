import argparse
import importlib.util
import json
import tempfile
import unittest
from pathlib import Path


MODULE_PATH = Path(__file__).with_name("run_cluster.py")
SPEC = importlib.util.spec_from_file_location("run_cluster", MODULE_PATH)
assert SPEC is not None and SPEC.loader is not None
CLUSTER = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(CLUSTER)


class RunClusterTest(unittest.TestCase):
    def test_hash_only_command_omits_debug_artifacts(self) -> None:
        args = argparse.Namespace(
            mimus=Path("mimus"),
            endpoint="http://127.0.0.1:1/v1",
            font=Path("font.ttf"),
            layout_model=Path("layout.onnx"),
        )
        command = CLUSTER.translation_command(args, Path("input.pdf"), Path("output.pdf"))
        self.assertNotIn("--debug", command)

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
            },
            {
                "paper": "failed",
                "producer": "Word",
                "published": False,
                "internal_errors": 1,
                "typed_degraded_paragraphs": None,
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
            },
        }
        rendered = CLUSTER.markdown(summary)
        self.assertIn("Internal/6: output_mismatch", rendered)
        self.assertIn("N/A", rendered)


if __name__ == "__main__":
    unittest.main()
