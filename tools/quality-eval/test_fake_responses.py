import importlib.util
import unittest
from pathlib import Path


MODULE_PATH = Path(__file__).with_name("fake_responses.py")
SPEC = importlib.util.spec_from_file_location("fake_responses", MODULE_PATH)
assert SPEC is not None and SPEC.loader is not None
FAKE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(FAKE)


class FakeResponsesTest(unittest.TestCase):
    def test_legacy_mode_keeps_frozen_outputs(self) -> None:
        self.assertEqual(FAKE.deterministic_translation("Hello 123 [36]"), "版数翻保稳试")
        self.assertEqual(FAKE.deterministic_translation("{v1} A <b2>B</b2>"), "{v1}试<b2>模</b2>")

    def test_conserving_mode_preserves_contract_tokens(self) -> None:
        source = "Model 28.4% at 20 ms [4,27], {v1} <b2>x</b2>."
        translated = FAKE.conserving_translation(source)
        for token in ("28.4%", "20", "ms", "[4,27]", "{v1}", "<b2>", "</b2>", "."):
            self.assertIn(token, translated)
        self.assertNotIn("Model", translated)
        self.assertTrue(any("\u4e00" <= char <= "\u9fff" for char in translated))

    def test_conserving_mode_is_deterministic(self) -> None:
        source = "d_model = 512; 1e-3 GHz"
        translated = FAKE.conserving_translation(source)
        self.assertEqual(translated, FAKE.conserving_translation(source))
        self.assertIn("1e-3 GHz", translated)

    def test_standalone_unit_letter_is_translated_as_prose(self) -> None:
        translated = FAKE.conserving_translation("Method A uses 20 A")
        self.assertNotIn("Method A", translated)
        self.assertTrue(translated.endswith("20 A"))


if __name__ == "__main__":
    unittest.main()
