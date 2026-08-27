# Test output fonts

`MimusTestGB2312-Regular.ttf` and `MimusTestGB2312-Bold.ttf` are test-only
derivatives of the OFL Noto Sans SC 2.004 fixture subsets. Their cmap covers
ASCII, GB2312 level-one Han characters, and common Chinese punctuation. The
small donor glyph set is intentionally reused: these assets test coverage,
subsetting, extraction, and asset injection, not visual font quality.

`MimusTestFallback-Regular.ttf` and `MimusTestFallback-Bold.ttf` are
deterministic subsets of the production DejaVu Sans 2.35 fallback files. They
contain only `U+2217`, `U+0141`, and `U+03F5` plus the required glyphs. Keeping
DejaVu's 2048 units-per-em and original advances is intentional: it covers the
PDF `/W` precision boundary that a 1000 units-per-em synthetic font cannot.

The pinned upstream files are from Matplotlib tag `v3.11.1`:

- Regular source SHA-256: `3fdf69cabf06049ea70a00b5919340e2ce1e6d02b0cc3c4b44fb6801bd1e0d22`
- Bold source SHA-256: `b184b89e3c1075f22f6b71575b6fc20d4972b3cfd3b23322ca6fd596dcaef167`
- fonttools: `4.63.0`

They are never referenced by production code or embedded in the release
binary. Tests pass their paths through the public output-font configuration.
Reproduce and compare two runs with:

```sh
python3 crates/mimus/tests/assets/fonts/generate_test_fonts.py \
  corpus/fonts/MimusCJK.ttf /tmp/regular-a.ttf "Mimus Test GB2312 Regular"
python3 crates/mimus/tests/assets/fonts/generate_test_fonts.py \
  corpus/fonts/MimusCJK.ttf /tmp/regular-b.ttf "Mimus Test GB2312 Regular"
cmp /tmp/regular-a.ttf /tmp/regular-b.ttf

python3 crates/mimus/tests/assets/fonts/generate_test_fonts.py \
  corpus/fonts/MimusCJKBold.ttf /tmp/bold-a.ttf "Mimus Test GB2312 Bold"
python3 crates/mimus/tests/assets/fonts/generate_test_fonts.py \
  corpus/fonts/MimusCJKBold.ttf /tmp/bold-b.ttf "Mimus Test GB2312 Bold"
cmp /tmp/bold-a.ttf /tmp/bold-b.ttf

pyftsubset DejaVuSans.ttf \
  --output-file=/tmp/fallback-regular-a.ttf \
  --unicodes=U+0141,U+03F5,U+2217 \
  --layout-features='' --no-hinting --glyph-names --legacy-cmap \
  --no-symbol-cmap --notdef-glyph --notdef-outline --recommended-glyphs \
  '--name-IDs=0,1,2,3,4,5,6' --no-name-legacy '--name-languages=0x409' \
  --no-harfbuzz-repacker --no-recalc-timestamp --canonical-order
pyftsubset DejaVuSans.ttf \
  --output-file=/tmp/fallback-regular-b.ttf \
  --unicodes=U+0141,U+03F5,U+2217 \
  --layout-features='' --no-hinting --glyph-names --legacy-cmap \
  --no-symbol-cmap --notdef-glyph --notdef-outline --recommended-glyphs \
  '--name-IDs=0,1,2,3,4,5,6' --no-name-legacy '--name-languages=0x409' \
  --no-harfbuzz-repacker --no-recalc-timestamp --canonical-order
cmp /tmp/fallback-regular-a.ttf /tmp/fallback-regular-b.ttf

pyftsubset DejaVuSans-Bold.ttf \
  --output-file=/tmp/fallback-bold-a.ttf \
  --unicodes=U+0141,U+03F5,U+2217 \
  --layout-features='' --no-hinting --glyph-names --legacy-cmap \
  --no-symbol-cmap --notdef-glyph --notdef-outline --recommended-glyphs \
  '--name-IDs=0,1,2,3,4,5,6' --no-name-legacy '--name-languages=0x409' \
  --no-harfbuzz-repacker --no-recalc-timestamp --canonical-order
pyftsubset DejaVuSans-Bold.ttf \
  --output-file=/tmp/fallback-bold-b.ttf \
  --unicodes=U+0141,U+03F5,U+2217 \
  --layout-features='' --no-hinting --glyph-names --legacy-cmap \
  --no-symbol-cmap --notdef-glyph --notdef-outline --recommended-glyphs \
  '--name-IDs=0,1,2,3,4,5,6' --no-name-legacy '--name-languages=0x409' \
  --no-harfbuzz-repacker --no-recalc-timestamp --canonical-order
cmp /tmp/fallback-bold-a.ttf /tmp/fallback-bold-b.ttf
```

The upstream licenses are preserved in `LICENSE-Noto-Sans-SC.txt` and
`LICENSE-DejaVu.txt`.

- Regular SHA-256: `510d0470ca8b77f035fe8e7143526207088c1bdad017451cf253020f72397d63`
- Bold SHA-256: `1a917349eb06866f5701532f0cea586d184edadbd1cfdd3f034f3a18f2ff5316`
- Fallback Regular SHA-256: `3634d4b65a151c61dcb82968f6a3bdc33435d062c4c69a5ea57e3db20122ac1e`
- Fallback Bold SHA-256: `d0f2fdc62e7cdf6e35c8b0629b19084917991603c0d51fe94109128176352b83`
