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
