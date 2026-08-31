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
  --font-fallback /path/to/DejaVuSans.ttf \
  --font-fallback-bold /path/to/DejaVuSans-Bold.ttf \
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

## L5-4R real-paper re-acceptance

- Date: 2026-08-28 (Asia/Shanghai)
- Issue: #110, under #34
- Base: `origin/master` at `250ddd3c8a2c6b0cba3a52e920f72271a7b3c1ab` plus the #110 walk fix
- Input SHA-256: `bdfaa68d8984f0dc02beaca527b76f207d99b666d31d1da728ee0728182df697`
- Layout model SHA-256: `45bf71750b00739a41fc209f132eb104a4d6b5bb29483c9078164d8b87cf28ba`
- Primary/bold font SHA-256: `d68bafcb48a2707749396aa12bbbd833cb70401f3a9a689fd2902c7e0d295964`
- Fallback regular/bold SHA-256:
  `3fdf69cabf06049ea70a00b5919340e2ce1e6d02b0cc3c4b44fb6801bd1e0d22` /
  `b184b89e3c1075f22f6b71575b6fc20d4972b3cfd3b23322ca6fd596dcaef167`
- Glossary: 96 entries, fingerprint
  `abc661f7ab8a80209e05adccf3cbf56418cf710a9fb0eddebe8945c9c001705a`
- Cache: byte copy of the archived L5-4 cache
  `592c911df60254659b29458c1e6870ad29173ac996ddce76a37ccbbdc8fefac9`; the archive itself was never
  opened for writing
- Output SHA-256: `5d9f97582b58a1ce415ed68aec1ddc9685c05cc53ed56bc91a22e2d6013ff70e`
- Result: **PASS**
- Evidence: `.context/real-pdf-test-2026-08-28-l5-4r/`

### API boundary

Fully offline. 146 translation jobs, 146 translation-cache hits, zero misses. Both the primary and
strict runs pointed at a loopback counting proxy with `--limit 0` whose upstream was a closed port;
both `/count` endpoints ended at `{"forwarded":0,"limit":0,"rejected":0}`. The process key was the
literal placeholder `sk-l5-4r-offline-fake-key`. **Real Responses calls: 0.**

### Root cause closed this round

The single L5-4 blocker `(12,69)` `Attention Visualizations` was not a genuine fit failure. The walk
parsed and validated a Form XObject `/BBox` and then discarded it, so it never applied the clip that
PDF 32000-1:2008 §8.10.2 Table 95 makes mandatory. Page 13 nests an Illustrator artwork form
(`/BBox [0 0 382.326 230.321]`) inside a `\includegraphics` wrapper (`/BBox [0 0 382.325 194.32]`);
the artwork draws `(Input-Input Layer5) Tj` at form-space `y = 217.04`, above the wrapper's clip
edge. No conforming renderer paints it, but the walk admitted its 18 glyphs as visible ink. Their
`visual_bbox` values and the `fallback_line` pseudo-region they clustered into sat directly on the
heading's own source footprint, so all nine font sizes from 11.9552 pt down to `MIN_FONT_SIZE_PT`
were rejected by `ink_bounds_are_safe`.

The fix intersects the transformed `/BBox` across form nesting and marks fully-outside glyphs
invisible while keeping them in the IL, so cross-engine alignment still sees the same character
sequence. Rotated or skewed forms take the axis-aligned superset of the transformed rectangle —
deliberately under-clipping rather than risking the removal of real ink. Clipped content is reported
once per page as `content_recovered` / `clipped_form_content` with the owning form object id.

### Five-round comparison

| Metric | L5 | L5-2 | L5-3 | L5-4 | L5-4R |
| --- | ---: | ---: | ---: | ---: | ---: |
| Date (2026-08) | 27 | 27 | 28 | 28 | 28 |
| Result | FAIL | FAIL | PASS | FAIL | **PASS** |
| Real Responses calls | 101 | 120 | 5 | 13 | **0** |
| Translate Han | 5,786 | 6,498 | 6,501 | 6,936 | 6,936 |
| Typeset Han | 5,506 | 5,209 | 6,294 | 6,930 | **6,936** |
| Poppler / MuPDF Han | 5,506 | 5,209 | 6,294 | 6,930 | **6,936** |
| Retention | 95.16% | 80.16% | 96.82% | 99.91% | **100.00%** |
| Han loss | 280 | 1,289 | 207 | 6 | **0** |
| Dominant typed loss | `unsupported_font` | `typeset_overflow` | `typeset_protocol` | `typeset_overflow` | none |
| Unexplained loss | 0 | 0 | 0 | 0 | 0 |
| Preserved paragraphs | 9 | 17 | 4 | 2 | **1** |
| Degraded pages | `[12,13,14]` | `[12,13,14]` | `[12,13,14]` | `[]` | **`[]`** |
| Suspicious echoes | not tracked | 1 | 1 | 1 | 1 |

The only remaining preserved paragraph is `(3,12)` `unreliable_unicode`; the only suspicious echo is
`(0,10)`, the author/email block. Both are reviewed, accepted residues carried unchanged since L5-3.

### Acceptance matrix

| Check | Evidence | Result |
| --- | --- | --- |
| Container | `qpdf --check` clean; 15 pages in and out; output starts with the exact input bytes | pass |
| Non-text structure | `/Form` 2,565, `/Image` 6, `/Link` 113, `/TrueType` 10, `/Type1` 24 all unchanged; only `/Type0` + `/CIDFontType2` subsets added; 22 page-outline mappings and 7 bookmarks unchanged | pass |
| Graphics identity | Canonical Form JSON SHA-256 `9fa117404f67aa2a1954c1259e6ba545308a1138e264634a41eb519b9971f2f1` identical in and out, same value as L5-4; XObject listings byte-identical | pass |
| Graphics render | Pages 1/13/14/15 at 150 DPI: every colored (non-greyscale) pixel is identical between input and output; no source-English overprint under the translations | pass |
| Chinese | Translate / Typeset / Poppler / MuPDF all 6,936 Han; retention 100.00%; Han loss 0 | pass |
| `(12,69)` | Both extractors contain 注意力可视化; the heading typesets with `single_line_bounds_expanded` `overflow_top_pt` 1.7455 | pass |
| Formula relocation | 52 exclusive spans, 4 shared spans, 79 characters, 30 units, 13 recovered mixed paragraphs, 0 residual — identical to L5-4 | pass |
| Policy passthrough | 7,978 selected characters over 153 table / 40 reference-content / 2 reference / 36 number / 1 header / 1 footer / 8 display-formula regions; one hash `4bc5cd6e3da4838395c094aacec610944f170590c8ead088153b95d969865074` from ParagraphFind through Write | pass |
| Placeholders | Zero `{vN}` / `{lN}` / `<bN>` residue in Poppler, MuPDF, and the Write IL | pass |
| Diagnostics | Only 5 `translation_identity` items dropped, inside that id's budget; no degradation, expansion, echo, or recovery item dropped | pass |
| Degradation | 0 degraded pages; 1 preserved paragraph `(3,12)`; 1 suspicious echo `(0,10)` | pass |
| Strict | Exit code 4, `strict_degradation`, lists exactly `(3,12)` and `(0,10)`, 0 calls, sentinel PDF hash unchanged, and no file created at an unused output path | pass |
| Security | No `sk-`-shaped token anywhere in the run directory, including the cache copy and the output PDF | pass |

### Comparison against the BabelDOC baseline

Page 13 was rendered at 150 DPI and compared with the BabelDOC translation of the same paper. The
baseline PDF is a visual reference only and is not stored in this repository. Heading ink bounding
boxes converted back to PDF points:

| Render | left | right | bottom | top |
| --- | ---: | ---: | ---: | ---: |
| English source | 108.00 | 230.40 | 709.92 | 718.56 |
| BabelDOC | 108.48 | 179.04 | 709.44 | 720.96 |
| mimus L5-4R | 108.48 | 179.04 | 709.44 | 720.48 |

Left, right, and bottom edges match the baseline exactly; the top differs by 0.48 pt, one 150 DPI
pixel of antialiasing. The attention-visualization figure, its rotated token labels, and the
translated caption are all intact.

### Disposition

Under the accepted baseline of BabelDOC parity, the real-paper translation is **达标**: zero Han
loss, zero degraded pages, zero overflow, an intact graphics layer, and heading placement that
matches the baseline. The remaining open work is tracked in #105-#108 and #38 and is not a blocker
for this judgement.

## L5-5R2 formula-gated acceptance

- Date: 2026-08-30; replacement accepted 2026-08-31 (Asia/Shanghai)
- Base stack: PR #138 plus the L5-5R2 conservation/formula-alignment layer
- Input SHA-256: `bdfaa68d8984f0dc02beaca527b76f207d99b666d31d1da728ee0728182df697`
- Replacement output SHA-256: `b3de6f10522f64a7e8bedba292c01d51724fb616f298bd4917ed8e54a475c0ef`
- Replacement cache SHA-256: `e5e825564ff2166c672db271c48745b1e467057ab8be09d51f4adca14f58e94c`
- Result: **PASS (replacement)**; the original `569095...b64a` acceptance remains withdrawn
- Evidence: `.context/vector-formula-fix/real6/`

The 2026-08-30 text-only audit is retained as a negative control: it omitted vector and raster formula
ink, so its `98.067441` score could not release the artifact. The replacement method is document-wide
and ink-closed. Every one of the 54 formula paragraphs has a published or ADR-0013 typed state and is
audited for unit completeness, order/adjacency, neighbor gap/inline hole, glyph replay, script
baseline, vector paths, and inline images. The replacement score is `97.988578`, with no confirmed
critical: FOR-04 is `0/71`, FOR-05 is `0/4`, FOR-02/FOR-03 are `0/0`, and CON-01 is `161/161`.

The screenshot regression at `(3,4)` now replays the detached radical and `d_k` under one page-space
delta. `(3,9)` likewise moves the numerator, fraction/radical paths, and operand as one rigid body;
`(4,21)` moves its radical overbar with the operand. Named rows remain regression samples, not a
substitute for the 54-row audit.

### Eight-round comparison

The first five columns retain the previously accepted historical counts. The last three use the
scorecard-v2 formula/CON contracts; their automatic totals should not be compared to pre-v2 scores.

| Metric | L5 | L5-2 | L5-3 | L5-4 | L5-4R | L5-5 | L5-5R | L5-5R2 |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| Result | FAIL | FAIL | PASS | FAIL | PASS | FAIL | FAIL | **PASS (replacement)** |
| Translate Han | 5,786 | 6,498 | 6,501 | 6,936 | 6,936 | 6,867 | 6,887 | **6,887** |
| Published Han | 5,506 | 5,209 | 6,294 | 6,930 | 6,936 | 6,858 | 6,878 | **6,878** |
| Preserved paragraphs | 9 | 17 | 4 | 2 | 1 | 2 | 2 | **2** |
| CON-01 | N/A | N/A | N/A | N/A | N/A | 94.83% | 99.39% | **100%** |
| FOR-01 proxy | N/A | N/A | N/A | N/A | N/A | 12 | 9 | **6 explained** |
| FOR-02 / FOR-03 | N/A | N/A | N/A | N/A | N/A | 1 / 1 | 1 / 1 | **0 / 0** |
| FOR-04 / FOR-05 | N/A | N/A | N/A | N/A | N/A | N/A | N/A | **0/71 / 0/4** |
| Title/author failures | N/A | N/A | N/A | N/A | N/A | 0 | 0 | **0** |
| Scorecard conclusion | N/A | N/A | N/A | N/A | N/A | critical-blocked | critical-blocked | **non-blocking** |

### Detection/execution closure

Four production gaps caused the withdrawn artifacts to remain blocked. First, extraction-order
superscript evidence did not attach the `2` in `(6,1)` to the existing model formula, so the
translation request could omit it. The boundary rule now accepts only a uniquely matched ASCII
numeric script proven by font size, baseline delta, and metric-box attachment. Second, MuPDF split
`(6,10)` into visually overlapping
lines; the scorecard's old top-edge heuristic excluded `epsilon` and `=`, then measured a false 23 pt
gap from the preceding prose. Production and scorecard now share the vertical-overlap rule from
`mimus-quality-contract`; the actual `=` to `10^-9` gap is 9 pt, below the source-derived 14.9439 pt
limit.

Third, formula relocation owned only text-show operands: fraction/radical paths could remain in the
source slot while glyphs moved. The walker now exposes bounded path/image spans, ownership requires a
unique complete graphics scope, and Typeset applies the glyph delta to the whole ink-closed unit or
preserves the paragraph as typed `typeset_protocol`. Fourth, extraction order could place a detached
radical in an earlier text segment. Whole-paragraph visual ownership now attaches it only when it
uniquely matches one existing model formula; formula existence remains model-owned. FOR-04 detects
source-slot residue, while FOR-05 independently detects missing or differently translated components.

The translation layer independently enforces CON-01 with the same lexer used by the scorecard: a
missing numeric/unit/reference token causes one corrective retry and a second failure preserves the
whole paragraph as typed `content_conservation`. Invalid responses are never cached. The final
20-paper conserving-fake sweep published 20/20 with zero Internal/6, zero conservation retry, and
zero `content_conservation` degradation across pdfTeX 8/8, XeTeX 4/4, LuaTeX 4/4, and Word 4/4.

### API and strict controls

The original L5-5R2 used seven real Responses calls. The vector/radical replacement used eight more
successful paragraph calls through the counting proxy (two in the first repair pass and six in the
conservation follow-up, including one corrective retry); term extraction remained zero. The user had
relaxed the earlier ten-call ceiling before these calls. The accepted primary replay is 137/137 cache
hits and zero provider calls. The accepted strict replay is also 137/137 hits, made zero calls through
a closed loopback endpoint, exited 4 with `strict_degradation`, listed only the two reviewed typed
paragraphs, and produced no output PDF.
