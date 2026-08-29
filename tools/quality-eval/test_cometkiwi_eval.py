import importlib.util
import unittest
from pathlib import Path


MODULE_PATH = Path(__file__).with_name("cometkiwi_eval.py")
SPEC = importlib.util.spec_from_file_location("cometkiwi_eval", MODULE_PATH)
assert SPEC is not None and SPEC.loader is not None
EVAL = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(EVAL)


class CometKiwiEvalTest(unittest.TestCase):
    def test_pairs_exclude_preserved_and_strip_formula_and_placeholders(self) -> None:
        source = {
            "pages": [{"index": 0, "paragraphs": [{
                "reading_order": 2,
                "text": {"chars": [
                    {"unicode": "A", "layout": {"label": "text", "policy": "translate"}},
                    {"unicode": "x", "layout": {"label": "inline_formula", "policy": "passthrough"}},
                    {"unicode": "1", "layout": {"label": "text", "policy": "translate"}},
                ]},
            }]}],
        }
        translated = {
            "pages": [{"index": 0, "paragraphs": [{
                "reading_order": 2,
                "translated_text": "译{v1}x1",
                "text": {"chars": []},
            }]}],
        }
        self.assertEqual(
            EVAL.paragraph_pairs(source, translated),
            [{"page_index": 0, "reading_order": 2, "source": "A 1", "translation": "译x1"}],
        )

    def test_percentile_is_linear_and_deterministic(self) -> None:
        self.assertEqual(EVAL.percentile([0.4, 0.1, 0.3, 0.2], 0.5), 0.25)


if __name__ == "__main__":
    unittest.main()
