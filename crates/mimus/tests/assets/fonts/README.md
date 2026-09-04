# Test output fonts

`MimusTestGB2312-Regular.ttf` and `MimusTestGB2312-Bold.ttf` are test-only
derivatives of the OFL Noto Sans SC 2.004 fixture subsets. Their cmap covers
ASCII, GB2312 level-one Han characters, and common Chinese punctuation. The
small donor glyph set is intentionally reused: these assets test coverage,
subsetting, extraction, and asset injection, not visual font quality.

The STIX test assets mirror the production routing contract:

- `MimusTestLatinText.ttf` is a variable subset of STIX Two Text 2.13 b171.
  It covers ASCII, `U+0141`, `U+03F5`, and `U+201C`, and retains the original
  Regular/Bold named instances.
- `MimusTestLatinSymbol.ttf` is a static subset of STIX Two Math 2.12 b168a
  containing `U+2217`, which STIX Two Text does not cover.
- `MimusTestLatin.ttf` is a compact STIX Two Math subset used at the public
  self-provided-font seam. It covers ASCII and all routing oracle characters.

The split Text/Math assets prove same-family symbol fallback. The combined
asset lets CLI tests provide both public Latin weight slots without requiring
an additional public symbol-font option.

`MimusTestVariable.ttf` is a deterministic five-character subset of the
production Noto Sans SC 2.004 variable font. It retains the original `fvar`,
`gvar`, `HVAR`, and named Regular/Bold instances so tests can prove that the
`wght=400` and `wght=700` user-coordinate locations drive both planning metrics
and embedded outlines. It contains only original glyphs; the generator never
synthesizes glyphs into the variation tables.

- Variable source SHA-256: `d68bafcb48a2707749396aa12bbbd833cb70401f3a9a689fd2902c7e0d295964`
- STIX Two Text source SHA-256: `7962b8b7811e6a896c9a91a0bccbb5241047770eb24d4997c5cb5fe21d5c0df2`
- STIX Two Math source SHA-256: `562551b15b836e6e01d1b7350909baf3c8c8d83260c1190fbf4544333e6936de`
- fonttools: `4.63.0`

They are never referenced by production code or embedded in the release
binary. Tests pass their paths through the public output-font configuration.
Reproduce and compare two runs with:

```sh
python3 crates/mimus/tests/assets/fonts/generate_test_fonts.py \
  NotoSansSC-VF.ttf /tmp/variable-a.ttf \
  --characters='M中文测试' --variable-subset
python3 crates/mimus/tests/assets/fonts/generate_test_fonts.py \
  NotoSansSC-VF.ttf /tmp/variable-b.ttf \
  --characters='M中文测试' --variable-subset
cmp /tmp/variable-a.ttf /tmp/variable-b.ttf

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

python3 crates/mimus/tests/assets/fonts/generate_test_fonts.py \
  'STIXTwoText[wght].ttf' /tmp/latin-text-a.ttf \
  --characters='Łϵ“' --ascii --variable-subset
python3 crates/mimus/tests/assets/fonts/generate_test_fonts.py \
  'STIXTwoText[wght].ttf' /tmp/latin-text-b.ttf \
  --characters='Łϵ“' --ascii --variable-subset
cmp /tmp/latin-text-a.ttf /tmp/latin-text-b.ttf

python3 crates/mimus/tests/assets/fonts/generate_test_fonts.py \
  STIXTwoMath-Regular.ttf /tmp/latin-symbol-a.ttf \
  --characters='∗' --variable-subset
python3 crates/mimus/tests/assets/fonts/generate_test_fonts.py \
  STIXTwoMath-Regular.ttf /tmp/latin-symbol-b.ttf \
  --characters='∗' --variable-subset
cmp /tmp/latin-symbol-a.ttf /tmp/latin-symbol-b.ttf

python3 crates/mimus/tests/assets/fonts/generate_test_fonts.py \
  STIXTwoMath-Regular.ttf /tmp/latin-a.ttf \
  --characters='Łϵ∗“' --ascii --variable-subset
python3 crates/mimus/tests/assets/fonts/generate_test_fonts.py \
  STIXTwoMath-Regular.ttf /tmp/latin-b.ttf \
  --characters='Łϵ∗“' --ascii --variable-subset
cmp /tmp/latin-a.ttf /tmp/latin-b.ttf
```

The Noto and STIX derivatives are licensed under the OFL 1.1 text preserved in
`LICENSE-OFL-1.1.txt`.

- Regular SHA-256: `510d0470ca8b77f035fe8e7143526207088c1bdad017451cf253020f72397d63`
- Bold SHA-256: `1a917349eb06866f5701532f0cea586d184edadbd1cfdd3f034f3a18f2ff5316`
- Latin Text SHA-256: `15253cedd8e67b26019900b09b048af23f3e4c1f2e0b352eeb50ccb39491d9a5`
- Latin Symbol SHA-256: `defb3cf75af1da832e26016183c6aff54985e580b24141bb5bbb48f32411c352`
- Combined Latin SHA-256: `621539180203f4667d247c49c8bf4102112b28e1627190ca625ebd1e61848a5f`
- Variable SHA-256: `a1105d5892eaad20ed1ad692827b06a7adc392f214a835740fa4d94bf5029ac5`
