# M3 Phase 2 · PP-DocLayoutV3 production validation

- Date: 2026-08-26
- Issue: #84
- Input: `1706.03762v7.pdf` (15 pages)
- Model: PP-DocLayoutV3 ONNX at commit
  `46bbdf188bb0a772c08aed74882ce7e51a8f1ea6`, SHA-256
  `45bf71750b00739a41fc209f132eb104a4d6b5bb29483c9078164d8b87cf28ba`
- Runtime: ort `2.0.0-rc.13`, CPU EP, 4 intra-op threads

All inference was local. Translation validation used either `backend none` or a Responses-compatible
echo server bound only to `127.0.0.1`; no real key or public translation API was used. HTTP proxies
were pointed at a closed loopback port. Evidence is in
`.context/m3-phase2-onnx-validation/`.

## Qualification

The env-gated test `m0_qualification_matches_archived_boxes_classes_and_reading_order` runs only
when `MIMUS_LAYOUT_MODEL` is set. It feeds the pinned M0 Poppler 200 DPI raster to the production
detector and requires the six `text` boxes and query order `23, 53, 125, 154, 230, 283` recorded by
M0 experiment 1. The test passed with the pinned model. An invalid or missing env path is a hard
failure, not a skip.

PDFium and Poppler antialiasing produce different scores/query ids even at the same DPI. Production
therefore renders at the M0 200 DPI contract, while the exact M0 oracle remains the archived raster;
the real-paper results below measure the production PDFium path separately.

## Real-paper labels

Production `inspect` completed for every page, with the existing page-level degradation described
below. Model-region counts on the 12 translatable pages were:

| Label | Regions | Policy result |
| --- | ---: | --- |
| `doc_title` | 1 | translate + bold |
| `paragraph_title` | 22 | translate + bold |
| `abstract` | 1 | translate |
| `footnote` | 1 | translate |
| `table` | 153 | passthrough |
| `display_formula` | 7 | passthrough |
| `inline_formula` | 72 | passthrough or paragraph placeholder |
| `reference` + `reference_content` | 41 | passthrough |
| `header` + `footer` + `number` | 32 | passthrough |
| `text` | 66 | translate |

Page 0 has a `doc_title`, `paragraph_title` + `abstract`, a `footnote`, and separate author blocks.
The author area improved from 28 interleaved fragments in the merged single-line baseline to eight
author/affiliation blocks; it is better ordered but not semantically perfect (one block still combines
Niki Parmar and Jakob Uszkoreit).

Nested inline formulas initially became standalone passthrough paragraphs. The production-shaped
red test exposed this, and paragraph reconstruction now attaches an inline formula to the smallest
larger translatable model region containing it. It does not attach formulas to table/header/display
containers. The final paper has 21 mixed text/formula paragraphs. The fake run emitted 59 `{vN}`
tokens across 24 requests (maximum five in one request), and echo restoration completed without a
placeholder violation.

## Baseline comparison

The fake replay used a fresh copy of the Phase 1 cache and the same 96-term glossary fingerprint
`abc661f7ab8a80209e05adccf3cbf56418cf710a9fb0eddebe8945c9c001705a`.

| Metric | Merged #85 + #86 baseline | ONNX + fake echo | Change |
| --- | ---: | ---: | ---: |
| translation jobs | 246 | 135 | -111 (-45.1%) |
| cache hits | 246 | 45 | old segmentation mostly invalidated |
| cache misses | 0 | 90 | requires a new authorized L5 |
| `math_passthrough` | 102 | 0 | heuristic now fallback-only |
| `typeset_overflow` | 36 | 0 | not a quality comparison; 90 misses echoed source |
| preserved paragraphs | 41 | 1 | only `unreliable_unicode` in fake run |
| degraded pages | 3 | 3 | unchanged |

The job reduction is driven by real labels: 42 paragraphs containing reference/reference_content,
table content, display formulas, formula numbers, page apparatus, and standalone formulas leave the
ordinary translation set. The old cache still hit only 45/135 jobs; 90 new segment keys missed.
This is the expected broad cache invalidation from model-owned paragraph reconstruction.

The overflow/preserved improvement must not be treated as Chinese quality evidence. Cache misses
were answered by an identity loopback, so those paragraphs did not exercise Chinese typesetting.
Only a new real-API run can establish the new overflow buckets and end-to-end quality.

The `backend none` and final fake outputs both passed `qpdf --check`, Poppler text extraction, and
MuPDF text extraction. `backend none` produced no math heuristic diagnostics or new preservation.

## Known deviation

Pages 12-14 retain the merged baseline's `bad_form_b_box` page degradation. Per ADR-0013, degraded
pages do not enter layout inference or paragraph reconstruction, so production IL cannot contain the
prompt's requested reference boxes on the last two pages. Page 11 does contain one `reference` and
40 `reference_content` model regions. Fixing malformed Form BBox handling is outside #84; silently
running those pages through the translatable path would violate the existing page-preservation
contract.

This also means the current reference job reduction is a lower-bound observation over the 12 pages
that reach layout, not validation of all bibliography pages.

## Recommended authorized L5

Use the pinned model and a brand-new cache; do not reuse either the old segmentation cache or any
fake-loopback working cache. Reuse the reviewed glossary and disable auto extraction so the run has a
stable glossary fingerprint and only paragraph translation calls:

```sh
MIMUS_PDFIUM_LIBRARY=/path/to/libpdfium.dylib \
MIMUS_OPENAI_API_KEY=... \
cargo run -p mimus -- --json translate 1706.03762v7.pdf \
  --backend openai --endpoint <approved-endpoint> --model <approved-model> \
  --target-language zh-CN \
  --layout-model /path/to/inference.onnx \
  --font /path/to/NotoSansSC-VF.ttf --font-bold /path/to/NotoSansSC-VF.ttf \
  --glossary glossary.toml --no-auto-terms \
  --cache l5-onnx-fresh.redb --concurrency 4 \
  --debug l5-onnx-debug --output 1706.03762v7.onnx.zh.pdf
```

Do not use `--strict` for the primary run: the known three degraded pages and one unreliable-Unicode
paragraph would intentionally block publication. After the non-strict artifact is reviewed, a strict
run can be used as the expected negative control. Required review should compare title/author/abstract,
the 24 placeholder-bearing requests, table/formula/reference passthrough, new overflow buckets, and
the unchanged degraded pages.

### Strict negative-control authorization

Invalid placeholder responses are intentionally not cached. Let `N` be the number of paragraphs
that still have a placeholder violation after their one semantic retry in the accepted non-strict
run. A strict replay may therefore have at most `N` translation-cache misses; every other paragraph
must be a cache hit. The authorization prompt must state this allowance explicitly instead of
claiming a zero-call replay.

One cache miss is not necessarily one Responses HTTP request: the placeholder policy retries the
first invalid response once. If all `N` paragraphs remain invalid, the strict replay can make up to
`2N` HTTP requests. The authorization must cap both quantities (`cache misses <= N`, Responses
requests `<= 2N`) and the run must stop if either cap is exceeded. A strict failure must still list
only the reviewed degradation set and must not create or overwrite an output file.
