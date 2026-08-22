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
