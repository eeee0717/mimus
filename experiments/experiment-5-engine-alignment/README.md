# Experiment 5: engine character alignment

This read-only experiment compares production operator-walk characters with the
owned PDFium character snapshots. It does not translate or write any PDF.

```bash
cargo run --release \
  --manifest-path experiments/experiment-5-engine-alignment/Cargo.toml -- \
  --pdfium-library /path/to/libpdfium.dylib \
  --output .context/engine-alignment/report.json \
  --tolerances 0.5 \
  /path/to/file.pdf /path/to/pdf-directory
```

The first tolerance is the primary run and includes bounded residual detail.
Later tolerances are aggregate-only sensitivity runs. Use a release build for a
large corpus because production `walk_page()` is intentionally unmodified and
can be slow on large content streams in a debug build.

Matching is staged:

1. Lock exact multiset matches within `0.001 pt`.
2. On remaining upright characters, reconcile same-Unicode candidates inside
   the requested tolerance when baseline and vertical boxes agree.
3. Accept a different-Unicode residual only when the geometry candidate is
   unique in both directions and a nearby matched sequence anchor supports it.
4. Record contiguous residual sequences with at least 75% Unicode agreement as
   sequence-only F evidence. They are not counted as geometric matches.

The report splits residuals into the decision matrix's A-F classes. It also
separates PDFium hyphen markers, UTF-16 surrogate halves, ligature expansion,
walk Unicode provenance, off-page characters, ambiguity and ordering evidence.
PDFium generated characters are excluded by the production `PdfiumEngine`
adapter. Raw reports and input PDFs belong under `.context`; do not commit them.

This runner is an evidence tool. It does not replace production
`validate_character_alignment`, set a production tolerance, or amend an ADR.
