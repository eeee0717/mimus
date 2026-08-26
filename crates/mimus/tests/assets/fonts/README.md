# Test output fonts

`MimusTestGB2312-Regular.ttf` and `MimusTestGB2312-Bold.ttf` are test-only
derivatives of the OFL Noto Sans SC 2.004 fixture subsets. Their cmap covers
ASCII, GB2312 level-one Han characters, and common Chinese punctuation. The
small donor glyph set is intentionally reused: these assets test coverage,
subsetting, extraction, and asset injection, not visual font quality.

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
```

The upstream SIL Open Font License 1.1 is preserved in
`LICENSE-Noto-Sans-SC.txt`.

- Regular SHA-256: `510d0470ca8b77f035fe8e7143526207088c1bdad017451cf253020f72397d63`
- Bold SHA-256: `1a917349eb06866f5701532f0cea586d184edadbd1cfdd3f034f3a18f2ff5316`
