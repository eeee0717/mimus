# Exact fixture font

`MimusExact.ttf` is a deterministic subset of DejaVu Sans 2.34 from TeX Live
2026. It contains `.notdef`, `.null`, `nonmarkingreturn`, space, and the Latin
capitals `C E I M R S T U`. The fixture writer embeds this file verbatim; it
does not run fonttools during a build.

The committed bytes were produced twice with fonttools 4.63.0 and compared by
SHA-256 before being pinned in each exact fixture manifest:

```sh
pyftsubset DejaVuSans.ttf \
  --output-file=MimusExact.ttf \
  --unicodes=U+0020,U+0043,U+0045,U+0049,U+004D,U+0052,U+0053,U+0054,U+0055 \
  --layout-features='' --no-hinting --glyph-names --legacy-cmap \
  --no-symbol-cmap --notdef-glyph --notdef-outline --recommended-glyphs \
  '--name-IDs=0,1,2,3,4,5,6' --no-name-legacy '--name-languages=0x409' \
  --no-harfbuzz-repacker --no-recalc-timestamp --canonical-order
```

SHA-256: `6e1e40974dce5dca579f3f191dd7dcc9953e6e04165d69f36d01aa8242a24735`

The upstream license is preserved in `LICENSE-DejaVu.txt`.

## Type1 fixture font

`MimusType1.pfb` is derived from the CMMI10 Type1 font shipped by TeX Live
2026. It is renamed to `MIMUST+CMMI10`, and its cleartext built-in encoding
maps code 65 to `/alpha`; the encrypted `/alpha` CharString and all other
glyph data remain unchanged. This gives the
FONT-08 fixture a real embedded Type1 program whose encoding is independent of
the PDF font dictionary.

Pinned source:

- file: `fonts/type1/public/amsfonts/cm/cmmi10.pfb` from TeX Live 2026;
- source SHA-256: `e3661061e8aa474d6de5ffa916edceb0e3d8b998862018c147f0357fce00bcd7`;
- t1utils: `t1disasm 1.42` and `t1asm 1.42`.

The source was disassembled, `/CMMI10` was renamed to `/MIMUST+CMMI10`, the
single line `dup 65 /A put` was changed to `dup 65 /alpha put`, and the font
was reassembled as binary PFB. The recipe was run twice and the outputs
compared byte-for-byte. Both outputs had
SHA-256 `ef2ecaff359f71078eb6611b9b4b2859d84666256340d5ee23a5657136773786`.
The upstream SIL Open Font License 1.1 is preserved in
`LICENSE-AMSFonts.txt`.

## CJK fixture font

`MimusCJK.ttf` is a deterministic Regular-weight subset of Noto Sans SC from
the official `notofonts/noto-cjk` Sans 2.004 release. It contains `.notdef`,
the recommended glyphs, space, the Latin capitals `I M S U`, and the Simplified
Chinese characters `\u4e2d \u6587 \u6d4b \u8bd5`. The fixture writer embeds the
committed subset verbatim and never downloads or subsets a font during a build.

Pinned upstream source:

- Repository commit: `523d033d6cb47f4a80c58a35753646f5c3608a78`
- File: `Sans/Variable/TTF/Subset/NotoSansSC-VF.ttf`
- Source SHA-256: `d68bafcb48a2707749396aa12bbbd833cb70401f3a9a689fd2902c7e0d295964`
- fonttools: `4.63.0`

The Regular instance and subset were produced with:

```sh
fonttools varLib.instancer NotoSansSC-VF.ttf wght=400 \
  --output NotoSansSC-Regular-full.ttf

pyftsubset NotoSansSC-Regular-full.ttf \
  --output-file=MimusCJK.ttf \
  --unicodes=U+0020,U+0049,U+004D,U+0053,U+0055,U+4E2D,U+6587,U+6D4B,U+8BD5 \
  --layout-features='' --no-hinting --glyph-names --legacy-cmap \
  --no-symbol-cmap --notdef-glyph --notdef-outline --recommended-glyphs \
  '--name-IDs=0,1,2,3,4,5,6' --no-name-legacy '--name-languages=0x409' \
  --no-harfbuzz-repacker --no-recalc-timestamp --canonical-order
```

The subset command was run twice from the same Regular instance and the two
files compared byte-for-byte. Both outputs had SHA-256
`a1677185f15e59c1ccb25e0fb320ab23d3a34d27649496eff089df41e27074ac`.
The upstream SIL Open Font License 1.1 is preserved in
`LICENSE-Noto-Sans-SC.txt`.

`MimusCJKBold.ttf` is the matching 700-weight instance with the same committed
glyph coverage. It is produced with the same commands above except
`wght=700`, and has SHA-256
`16a829fddcd44df524ffc64cf22d64fbf7259919ff64ceeea3c6d47e14df21bb`.
These committed Regular/Bold files are input fixture assets only. Production
output fonts are resolved at runtime and are never compiled from this
directory. Output-font tests use separate assets under
`crates/mimus/tests/assets/fonts/` through the same path/config injection used
by the CLI.

## M3 layout and mixed-metric fixture fonts

`MimusMath.ttf` is a deterministic subset of the same DejaVu Sans 2.34 source
used by `MimusExact.ttf`. It contains U+2211 and scales that glyph's outline
threefold in the vertical direction while retaining the source font's
ascender, descender, and horizontal advance. This creates the legal
metric-versus-ink stress shape required by LAYOUT-04 without involving the
production PDF or font stack.

Pinned source:

- TeX Live 2026 `fonts/truetype/public/dejavu/DejaVuSans.ttf`;
- source SHA-256: `08ca98e69d9d8fa1065584b4f9ab7d49b6205abea6572b90e171b254845bb990`;
- fonttools 4.63.0.

Reproduce twice and compare with:

```sh
python3 corpus/fonts/generate_mimus_math.py DejaVuSans.ttf /tmp/math-a.ttf
python3 corpus/fonts/generate_mimus_math.py DejaVuSans.ttf /tmp/math-b.ttf
cmp /tmp/math-a.ttf /tmp/math-b.ttf
```

SHA-256: `d6dd910115e530ed76ca032c13bafde8d52e0725181bcb1fc59be6496a91b926`

`MimusTermes.otf` and `MimusCursor.otf` are deterministic U+0020/U+0043/
U+0049/U+004D/U+0053/U+0055 subsets of TeX Gyre Termes and Cursor 2.004.
Their internal and PDF descriptor metrics are pinned to the independently
specified TYPE-11 Times/Courier-compatible values, respectively 783/-216 and
814/-300 units. The source SHA-256 values are respectively
`cc3fe7c707b81428d23d54df3eadd9228a2bf6a4d43125d94df56f5f63134659`
and `0667deb48aa0e88be8f499c4d308e8b9116f290e7f969b0f5a34ee15c9644272`.

Both subsets use fonttools 4.63.0. Reproduce twice per source and compare with:

```sh
python3 corpus/fonts/generate_mimus_metric_font.py \
  texgyretermes-regular.otf /tmp/termes-a.otf \
  "Mimus Metric Termes" MimusMetricTermes 783 -216
python3 corpus/fonts/generate_mimus_metric_font.py \
  texgyretermes-regular.otf /tmp/termes-b.otf \
  "Mimus Metric Termes" MimusMetricTermes 783 -216
cmp /tmp/termes-a.otf /tmp/termes-b.otf

python3 corpus/fonts/generate_mimus_metric_font.py \
  texgyrecursor-regular.otf /tmp/cursor-a.otf \
  "Mimus Metric Cursor" MimusMetricCursor 814 -300
python3 corpus/fonts/generate_mimus_metric_font.py \
  texgyrecursor-regular.otf /tmp/cursor-b.otf \
  "Mimus Metric Cursor" MimusMetricCursor 814 -300
cmp /tmp/cursor-a.otf /tmp/cursor-b.otf
```

The DejaVu license remains `LICENSE-DejaVu.txt`; the TeX Gyre subsets use the
GUST Font License preserved in `LICENSE-GUST-Fonts.txt`.

- `MimusTermes.otf`: `efe5361d55b776d098ce7bdfbc9ec04e75b38e0339fed8efbb4502c2aeb133f7`
- `MimusCursor.otf`: `10db5aa979b0145e2417cd24c8e181b2c62c6e18a3680cd63859479ce6327420`
