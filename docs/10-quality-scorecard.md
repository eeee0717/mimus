# M3 quality scorecard

Status: accepted measurement taxonomy; release thresholds below are a **proposal, pending user approval**.

## 1. Scope and evidence hierarchy

The scorecard measures a completed translation from public artifacts: protocol-v2 NDJSON, debug IL
snapshots, the input PDF, and the incrementally written output PDF. It is an evaluator, not part of
the translation pipeline, and must not depend on `mimus-core`. A score is a triage signal, never a
substitute for human bilingual or visual review.

Evidence follows ADR-0013 and ADR-0015. The operator walk and its source spans own write semantics;
typed degradation is valid evidence rather than an exception to hide. PDFium observations are
cross-evidence, not Unicode truth. PDF structure and raster checks are evaluated independently of
the implementation under test.

Every error has severity weight `critical = 10`, `major = 5`, or `minor = 1`. For dimension `d`,
`E_d = sum(count_i * weight_i)`, `R_d = 1000 * E_d / max(1, non-whitespace output Unicode scalar
count from pinned Poppler extraction)`, and
`S_d = clamp(100 - R_d, 0, 100)`. The total is the unweighted arithmetic mean of the six capped
dimension scores. Counts, rates, formula IDs, hashes, and binary evidence remain in the JSON so a
single total cannot conceal the cause. A human-confirmed critical defect sets the report conclusion
to `blocked_by_confirmed_critical` regardless of the numeric total. It does not silently rewrite the
six-dimensional score.

Subclasses are append-only within schema v2. Changing a formula or removing a subclass requires a
schema-version change and a fresh baseline.

## 2. Dimensions and formulas

### 2.1 Coverage gap (`coverage_gap`)

Coverage asks whether content eligible under the policy reached a publishable translated state.

| ID | Measurement | Formula | Severity | Source |
| --- | --- | --- | --- | --- |
| COV-01 | paragraph coverage | translated, non-preserved eligible paragraphs / eligible paragraphs | major per missing paragraph | ParagraphFind policy + Translate `translated_text` + typed `preserved` |
| COV-02 | Han-weighted coverage | Han scalars in publishable Typeset text / Han scalars in Translate text | major per missing paragraph; ratio reported separately | Translate + Typeset IL |
| COV-03 | typed reason distribution | count by ADR-0013/0017 reason | descriptive; COV-01 carries weight | Translate/Typeset IL and summary |

This is deliberately policy-aware. Passthrough tables, formulas, references, page apparatus,
non-upright units, and invisible content are not false coverage gaps. For real input,
`Internal/6` is always a defect: the only valid outcomes are publication or a bounded typed
degradation classified by ADR-0013.

### 2.2 Overtranslation (`overtranslation`)

| ID | Subclass | Formula | Severity | Source |
| --- | --- | --- | --- | --- |
| OVR-01 | policy passthrough changed | non-identical translation attached to a non-translate policy paragraph | critical | ParagraphFind + Translate IL |
| OVR-02 | superscript/affiliation marker | changed short paragraph at <= 8 pt with a compact baseline band | major | char font size/baseline + Translate IL |
| OVR-03 | number/page marker | changed paragraph containing only digits and numbering punctuation | major | source chars + Translate IL |
| OVR-04 | blank separator | changed paragraph whose source is empty or whitespace only | major | source chars + Translate IL |

OVR-02 requires a character at most 80% of its paragraph's median size and a baseline shift of at
least 15% of the median size. It is a candidate detector, not proof: author affiliations and
mathematical exponents require human adjudication. Confirmed issues retain page/paragraph/character
evidence.

### 2.3 Mistranslation risk (`mistranslation_risk`)

Fake translation cannot establish meaning. This dimension therefore reports risk proxies only.

| ID | Subclass | Formula | Severity | Source |
| --- | --- | --- | --- | --- |
| RSK-01 | weak reliability admitted | paragraphs containing unresolved walk Unicode | major | ParagraphFind char `unicode` and typed summary |
| RSK-02 | placeholder violation | final validator violation diagnostics | critical | NDJSON diagnostic |
| RSK-03 | suspicious echo | summary suspicious-echo entries | major | NDJSON summary |
| CON-01 | numeric/unit/reference conservation | exact target multiset contains every eligible source token occurrence | critical per missing occurrence | StylesAndFormulas + Translate IL |
| CON-02 | glossary consistency | canonical target occurrences / source-term occurrences | major per missing canonical occurrence | versioned glossary + StylesAndFormulas + Translate IL |
| FOR-01 | formula-unit completeness proxy | unbalanced translated delimiter paragraphs + suspicious translated fragments immediately adjacent to model-labelled formula spans | critical per occurrence | StylesAndFormulas + Translate IL |

CON-01 uses one conservative lexer on both sides. It recognizes signed integers, decimals,
percentages and scientific notation; a fixed unit vocabulary only when a unit follows numeric
context; and bracketed numeric references such as `[36]` or `[4,27,28,22]`. Tokens are compared as
exact multisets, so `4` is not satisfied by `40`. Number words and semantic unit conversions are out
of scope. Only source characters that match production translation eligibility participate:
visible, upright, `translate` policy, and owned by one of the page's direct `/Contents` objects.
The direct object set comes from structured pinned `qpdf` page JSON; text reached through Form
XObjects is not mistaken for request input. Tokens are lexed within continuous eligible runs after
StylesAndFormulas has finalized formula membership; skipped formula/passthrough/Form runs form
boundaries, while spaces inside a
run follow the public IL `implicit_space_before` contract used by production `request_text()`. The
evaluator never infers boundaries from geometry.

CON-02 consumes the same version-1 TOML glossary used by the evaluator. Each source occurrence must
have the glossary's canonical target in its aligned translated paragraph. This makes multiple
renderings observable as missing canonical occurrences without guessing unlisted synonyms.

CON-01 and CON-02 are applicable to real output and the conserving fake profile. They are explicitly
`not-applicable` for legacy fake output, which intentionally discards the source tokens. CON-02 is
also `not-applicable` when no glossary is supplied. FOR-01 is a v2 mechanical proxy: it checks
delimiter balance and short numeric, closing-delimiter, underscore, or known `model` fragments
touching a model-labelled formula span. Once the formula repair round emits typed unit diagnostics,
the proxy must be upgraded to exact unit membership rather than tuned against this baseline.

No risk score may be described as translation accuracy. Real mistranslation still requires human
bilingual review or a separately governed semantic evaluator.

### 2.4 Layout drift (`layout_drift`)

| ID | Subclass | Formula | Severity | Source |
| --- | --- | --- | --- | --- |
| LAY-01 | position offset | Euclidean distance between source and replacement bbox lower-left; report median | major above 1 pt | ParagraphFind/Typeset bounds |
| LAY-02 | footprint overlap | rectangle intersection-over-union; report median | major below 0.80 | same |
| LAY-03 | font scaling | median output/source char font-size ratio | descriptive until output glyph geometry is public | IL char font size |
| LAY-04 | bounds expansion | count `single_line_bounds_expanded` and `multi_line_bounds_expanded` | minor | NDJSON diagnostics |
| LAY-05 | line-count change | output lines minus aligned source lines | major | pinned `pdftotext -bbox-layout`; reserved in schema v2 |
| FOR-02 | formula-neighbor continuity | nearest same-line output gap exceeds `max(2 * source word-gap median, 1.5em)` | major per excessive neighbor gap | ParagraphFind IL + pinned MuPDF `stext.json` output extraction |
| FOR-03 | unexplained inline hole | each FOR-02 excessive gap; area = gap width times extracted formula-line height | major per hole | same |

LAY-05 is reserved because current debug IL does not expose final output line boxes. The comparator
ledger may compute it from pinned extractor XML; the harness must not invent it from string length.

FOR-02 derives its bound per source paragraph. Source word-spacing samples are positive finite source
whitespace-character widths plus same-baseline `implicit_space_before` gaps whose characters on both
sides are `text/translate`; formula-adjacent gaps are excluded. The median is doubled and compared
with `1.5` times the paragraph median font size. A neighbor is required only where the source formula unit immediately touched a translatable
character on the same baseline. The output formula is matched by compact exact text and expected
vertical position before its nearest left/right extracted lines are measured. Unmatched formula
units remain visible in `formula_units` versus `matched_units`; v2 does not invent geometry for them.
FOR-03 intentionally shares FOR-02's derived bound, matching the adjudicated single continuity
contract for fixed-slot and relocation paths.

### 2.5 Typesetting lint (`typesetting_lint`)

| ID | Subclass | Formula | Severity | Source |
| --- | --- | --- | --- | --- |
| LNT-01 | Chinese kinsoku | paragraph starts with closing punctuation or ends with opening punctuation | major | translated text |
| LNT-02 | isolated punctuation | one-scalar punctuation paragraph | minor | translated text |
| LNT-03 | placeholder residue | `{v`, `{l`, or `<b` token prefix remaining | critical | translated text + extracted PDF text |
| LNT-04 | abnormal whitespace | leading/trailing or doubled ASCII whitespace | minor | translated text |
| LNT-05 | English residue | suspicious echo count; lexical ratio is reserved until language segmentation is pinned | minor | summary |

### 2.6 Structural fidelity (`structural_fidelity`)

| ID | Subclass | Formula | Severity | Source |
| --- | --- | --- | --- | --- |
| STR-01 | valid container and page count | qpdf input/output checks pass and page counts match | critical per failure | pinned qpdf |
| STR-02 | incremental-write prefix | output bytes begin with every input byte | critical | raw bytes |
| STR-03 | non-text object inventory | recursive qpdf JSON counts of Form, Image, Link, Annot, Outlines match | critical | pinned qpdf JSON |
| STR-04 | masked non-text pixels | identical RGB pixels / compared pixels after masking translated bboxes plus 2 pt | critical; `10 * (1-fidelity)*1000` | pinned Poppler PPM at 72 DPI |
| STR-05 | title/complete-author-block conservation | page-0 title and every paragraph between title and abstract/first paragraph title are policy passthrough and write-IL identical, with canonical source/write hashes | critical per failed binary invariant | ParagraphFind + Write IL |

The pixel check intentionally excludes text replacement footprints. It is sensitive to antialiasing
and renderer versions, so comparisons must pin `pdftoppm`; it complements rather than replaces the
object and span checks.

STR-05 is applicable only when both page-0 anchors exist. It reports four binary invariants: title
policy, complete visual author-block policy, title identity, and author-block identity. Identity
includes Unicode, source code, font, font size, baseline, metric box, visual bbox, passthrough
payload, and absence of a non-identity translation. Canonical SHA-256 values bind the complete
selected source and Write blocks as evidence without adding another weighted invariant. Missing
anchors are fail-closed to `not-applicable`, not a guessed author range.

### 2.7 Semantic QE sidecar

Reference-free QE is separate from the six capped dimensions. `tools/quality-eval/cometkiwi_eval.py`
aligns eligible non-preserved ParagraphFind/Translate IL paragraphs, strips placeholders and exact
IL-labelled formula units, collapses whitespace, and scores pairs in `(page_index, reading_order)`
order on CPU. The sidecar contains model source, revision, checkpoint SHA-256, full snapshot-tree
SHA-256, paragraph count, min/p10/median, and the lowest-N source/translation pairs.

The current public model is `Unbabel/wmt20-comet-qe-da`: the requested
`Unbabel/wmt22-cometkiwi-da` repository returned HTTP 401 under anonymous access on 2026-08-30.
QE scores are model-dependent triage signals, not translation accuracy, and no threshold is accepted;
the sidecar records `proposal-pending-user-adjudication`.

### 2.8 Process and cluster formulas

| ID | Measurement | Formula / status |
| --- | --- | --- |
| PRO-01 | publication and Internal | terminal `result` presence; Internal error count; real `Internal/6` is always a bug |
| PRO-02 | typed degradation | `degradation_summary.preserved_paragraph_count`; cluster median/worst and producer strata |
| PRO-03 | translation process | calls / eligible paragraph; retry diagnostics / calls; suspicious echoes / eligible paragraph; cache hits / (hits + misses) |
| PRO-04 | resources | `/usr/bin/time -lp` wall seconds and maximum RSS; per-page timing is N/A until public artifacts expose it |
| PRO-05 | reproducibility | two selected papers rerun; published PDF SHA-256 must be byte-identical |

## 3. Deterministic harness

Run:

```sh
cargo run --locked --offline -p scorecard -- measure \
  --ndjson run.ndjson --debug-dir run-debug \
  --input-pdf input.pdf --output-pdf output.pdf \
  --json-out scorecard.json --markdown-out scorecard.md \
  --evaluation-profile real \
  --semantic-evaluation semantic-qe.json \
  --confirmed-critical 'human-reviewed defect'
```

The result body contains no timestamp, host path, random value, or tool stderr. Keys are ordered,
floating values are rounded to six decimals, and input/output SHA-256 values bind the evidence.
Temporary render paths do not enter the result. Required external tools are `qpdf`, `pdftotext`,
`pdftoppm`, and `mutool`. Cargo commands use `--locked --offline`. The conserving fake server is a
loopback-only test tool and does not alter the legacy fake default.

## 4. Proposed release line

This is a **proposal, pending user approval**, and is not a CI gate:

- paragraph and Han-weighted coverage both at least 95%; every remainder has a typed reason;
- zero OVR-01 policy changes and zero confirmed OVR-02/OVR-03 defects;
- zero placeholder violations or residue; every suspicious echo human-reviewed;
- 100% CON-01 conservation and zero FOR-01 formula proxy violations;
- median replacement IoU at least 0.80 and median offset at most 1 pt, with all expansions reviewed;
- zero FOR-02/FOR-03 continuity failures and all expected formula units matched;
- all four STR-05 title/author invariants pass where the anchor contract applies;
- qpdf, page count, byte-prefix, object inventory, formula/policy span checks all pass;
- masked non-text pixel fidelity is 100% on the pinned renderer;
- no `Internal/6` on real input.

Release-line changes require a user decision, a rationale, and a baseline rerun; thresholds must not
be loosened merely to make a failing corpus pass.

## 5. Current and historical baselines

The current 21-paper schema-v2 baseline, producer aggregates, 1706 QE review pack, and reproducibility
results are recorded in [11-quality-scorecard-v2-baseline.md](11-quality-scorecard-v2-baseline.md).
The remaining tables in this section preserve the schema-v1 baseline at `017d910` as historical
evidence. Its totals are not comparable to v2 without their original formula IDs and applicability
profile.

The anchor is the cached real L5-4R translation; the other 20 rows are deterministic fake-backend
outputs from the archived producer-stratified sweep. Fake scores measure pipeline exposure and risk,
not semantic translation accuracy. `C/O/R/L/T/S` are coverage, overtranslation, mistranslation-risk,
layout, typesetting-lint, and structure scores.

| Paper | Producer | C | O | R | L | T | S | Total |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 1706 L5-4R | anchor | 99.74 | 91.34 | 99.48 | 99.69 | 99.69 | 99.90 | 98.30 |
| 01 Adam | pdfTeX | 81.75 | 100.00 | 81.75 | 100.00 | 99.97 | 100.00 | 93.91 |
| 02 ResNet | pdfTeX | 88.00 | 99.79 | 88.10 | 99.90 | 99.10 | 99.92 | 95.80 |
| 03 SqueezeNet | pdfTeX | 87.65 | 98.82 | 89.61 | 99.96 | 99.02 | 99.96 | 95.84 |
| 04 MobileNets | pdfTeX | 87.67 | 99.29 | 87.67 | 99.93 | 99.96 | 99.98 | 95.75 |
| 05 BERT | pdfTeX | 85.29 | 99.39 | 85.29 | 99.94 | 99.47 | 99.98 | 94.89 |
| 06 DDPM | pdfTeX | 87.99 | 100.00 | 87.99 | 99.83 | 99.61 | 100.00 | 95.90 |
| 07 ViT | pdfTeX | 90.25 | 99.91 | 90.43 | 99.96 | 99.82 | 99.99 | 96.73 |
| 08 LoRA | pdfTeX | 89.30 | 99.55 | 89.45 | 99.97 | 99.83 | 99.98 | 96.35 |
| 09 Repliable onion routing | XeTeX | 73.81 | 99.44 | 73.81 | 99.71 | 99.75 | 99.99 | 91.08 |
| 10 Compact IBE | XeTeX | 88.46 | 95.12 | 92.03 | 99.84 | 99.97 | 99.99 | 95.90 |
| 11 SDitH hardware | XeTeX | 94.13 | 93.79 | 94.13 | 99.84 | 99.21 | 99.98 | 96.85 |
| 12 Hertz side channel | XeTeX | 99.71 | 73.63 | 99.71 | 99.12 | 99.77 | 99.87 | 95.30 |
| 13 Information-theoretic MPC | LuaTeX | 97.71 | 87.78 | 99.24 | 99.59 | 100.00 | 99.95 | 97.38 |
| 14 Masked comparisons | LuaTeX | 87.09 | 99.17 | 87.09 | 99.89 | 99.90 | 100.00 | 95.52 |
| 15 LWE two-step | LuaTeX | 85.22 | 97.37 | 85.90 | 99.94 | 99.98 | 100.00 | 94.74 |
| 16 Supersingular orientations | LuaTeX | 76.12 | 99.02 | 78.53 | 99.91 | 99.93 | 100.00 | 92.25 |
| 17 Informational consciousness | Word | 100.00 | 76.12 | 100.00 | 100.00 | 81.56 | 88.81 | 91.08 |
| 18 Consciousness model | Word | 100.00 | 31.29 | 100.00 | 99.28 | 92.37 | 91.85 | 85.80 |
| 19 Multibeam IoT | Word | 100.00 | 34.79 | 100.00 | 100.00 | 78.79 | 89.07 | 83.78 |
| 20 Tuberculosis biosensor | Word | 100.00 | 73.20 | 100.00 | 100.00 | 92.58 | 99.84 | 94.27 |

Producer aggregates:

| Producer | Papers | Mean total | Mean paragraph coverage | `unreliable_unicode` | Overtranslation candidates | Mean masked pixel fidelity |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| pdfTeX | 8 | 95.65 | 31.09% | 867 | 25 | 99.9892% |
| XeTeX | 4 | 94.78 | 68.69% | 638 | 186 | 99.9896% |
| LuaTeX | 4 | 94.97 | 48.56% | 614 | 98 | 99.9963% |
| Word | 4 | 88.73 | 100.00% | 0 | 363 | 99.9849% |

All 21 inputs and outputs pass qpdf, page-count, incremental-prefix, and non-text object inventory
checks. Strict pixel equality is not claimed: masked fidelity ranges from 99.9626% to 100%, and the
unmasked edge differences remain baseline evidence for later adjudication. The 20-paper total has
2,119 `unreliable_unicode` preserved paragraphs in the current post-fix artifacts; this is a different
denominator from #118's original 2,997-paragraph clustering dataset and must not replace that ledger.

## 6. Historical 1706 versus BabelDOC ledger

Inputs are the 1706 source SHA above, L5-4R output SHA
`5d9f97582b58a1ce415ed68aec1ddc9685c05cc53ed56bc91a22e2d6013ff70e`, and the local BabelDOC
v0.6.3 reference. The BabelDOC PDF and all paper bytes remain out of the repository.

| Candidate | Evidence and adjudication | Dimension | Disposition |
| --- | --- | --- | --- |
| mimus translates title; BabelDOC leaves English | mimus bbox `(211.488,148.828)-(396.504,164.686)` versus source `(211.488,150.164)-(399.893,165.641)`; product policy changed on 2026-08-29 | STR-05, critical | old judgment reversed: mimus fails typed passthrough policy; BabelDOC retaining English is not its defect |
| BabelDOC translates the red permission block; mimus leaves it English | source is model `text` / `translate`, but L5-4R publishes identity without typed degradation | COV-01 / RSK-03, major | #124 |
| BabelDOC translates the conference footer; mimus leaves it English | mimus `footer` policy is explicit passthrough | coverage | policy-conforming, not a defect |
| company/author superscripts | mimus changes `Ashish Vaswani* / Google Brain` to `* Ashish Vaswani / 谷歌大脑`; `*`, `†`, `‡` move and affiliation entities change. BabelDOC corrupts Toronto as `†多伦`, so it is corroborating evidence only | OVR-02, major | #125 |
| author-grid line and column drift | Ashish changes 3 lines to 2, left edge drifts -1.832 pt and height -8.859 pt; Noam email splits at `.`, and Niki/Jakob become one extraction block | LAY-01 / LAY-05, major | #126 |
| Chinese title/body weight | NotoSansSC variable font renders materially thinner than source/BabelDOC bold text | typesetting style, minor | existing #113 |
| abstract position | heading source `(283.758,386.532)-(328.243,397.280)`, mimus `(283.758,382.826)-(307.668,397.172)`, BabelDOC `(283.866,384.739)-(307.776,401.906)`; Chinese width change is expected, vertical shifts are measurable | LAY-01, minor candidate | retained in baseline; no new issue pending broader sample |
| page 13 attention heading | mimus and BabelDOC left/right/bottom match; top differs 0.48 pt at 150 DPI; figure and rotated labels remain intact | LAY-01, minor | accepted render quantization, no defect |

Under schema v1, the anchor measured 99.17% paragraph coverage (119/120), 100% Han-weighted coverage (6,936/6,936),
one typed `unreliable_unicode`, one suspicious echo, six expansions, zero placeholder residue, and
99.9801% masked pixel fidelity. Its score does not erase the three human-confirmed defects above.

## 7. Recovery-round input

For #118, preserve this schema and require before/after movement in COV-01/COV-02/RSK-01 by producer
and by the existing 23 recovery buckets. The proposed target is to recover at least the conservative
1,639/2,997 (54.7%) clustered paragraphs without increasing OVR-01, placeholder violations, typed
Internal failures, or structure failures; remeasure the exact same corpus denominator.

The data favors a #118 repair round before #38 gate engineering: the three TeX producer layers still
contain 2,119 weak-Unicode preserved paragraphs in these final-run artifacts, whereas #38 remains a
known 1/46 fixture-coverage gap. After recovery, rerun the matrix; if realized recovery is materially
below the 54.7% estimate, use the per-bucket residuals to choose fixtures instead of filling all 45
gaps indiscriminately.

## 8. Adjudication log

| Date | Decision | Consequence |
| --- | --- | --- |
| 2026-08-29 | Reverse the earlier title judgment: `doc_title` and the complete visual author block must be typed passthrough. LLM identity is not a policy contract. | A translated title is now a STR-05 failure. Production implementation remains in the separate formula/title repair stack. |
| 2026-08-29 | Formula-boundary leakage (`value ls]`) is a human-confirmed critical content defect. Fixed-slot and relocated formula continuity share one oracle. | FOR-01 is critical; FOR-02 and FOR-03 use one derived bound. The 1706 conclusion is blocked even if the numeric total is high. |
| 2026-08-29 | Human-confirmed critical defects override automatic totals. | Reports retain the numeric score for diagnosis but cannot conclude release eligibility while `confirmed_criticals` is non-empty. |
| 2026-08-30 | A real `Internal/6` is always a production bug. | Cluster rows remain failed/N/A; they may not be relabelled as typed degradation or assigned fabricated scores. |
| 2026-08-30 | During the formula/title stack rebase, schema-v1 verdict and STR-06 were classified as duplicates of schema-v2 conclusion and STR-05. The old identifiers are not retained. | STR-05 absorbs only the unique dual-box identity and canonical source/write hash evidence, with its four existing weighted invariants unchanged. |
