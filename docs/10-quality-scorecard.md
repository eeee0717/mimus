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
| FOR-04 | orphan source ink | formula glyphs leave their source slot while a neighboring vector path or inline image remains at the same input/output world coordinates | critical per occurrence | public StylesAndFormulas/Typeset IL + pinned MuPDF input/output `trace` |
| FOR-05 | formula rigid-body integrity | every source formula glyph and associated vector/inline-image component must reappear under one identical page-space translation | critical per formula unit | public StylesAndFormulas/Typeset IL + pinned MuPDF input/output `trace` |

CON-01 uses one conservative lexer on both sides. It recognizes signed integers, decimals,
percentages and scientific notation; a fixed unit vocabulary only when a unit follows numeric
context; and bracketed numeric references such as `[36]` or `[4,27,28,22]`. Ordinary tokens are
compared as exact multisets, so `4` is not satisfied by `40`. The lexer canonicalizes only
lexically explicit localized quantities: valid comma grouping; attached `K/M/B` magnitude suffixes
(`40K`), English `thousand/million/billion`, Chinese `万/亿`, and explicit Arabic or Chinese
fractions (`1/4` / `四分之一`). Thus `36M` equals `3600 万` and `4.5 million` equals `450 万`.
Whitespace keeps an otherwise ambiguous `K` in the unit vocabulary (`40 K` is Kelvin), and no
unmarked number-word or inferred semantic conversion is accepted. Latin magnitude/unit lexemes end
before a non-ASCII script, so `40K训练` is tokenized as `40K` plus Han text, while `40KB` remains
the longer byte unit. Only source characters that
match production translation eligibility participate:
visible, upright, `translate` policy, and owned by one of the page's direct `/Contents` objects.
The direct object set comes from structured pinned `qpdf` page JSON; text reached through Form
XObjects is not mistaken for request input. Tokens are lexed after StylesAndFormulas has finalized
formula membership. Accepted translations, including local and backend identities, carry additive
`translation_conservation` evidence in the Translate IL snapshot. `request_sha256` and
`response_sha256` bind the raw prepared request and validated response without duplicating either
string. Source and target multisets are
computed from the exact runtime semantic segments: bold spans remain continuous text, while each
formula placeholder is a hard boundary on both sides. Each side records its total distinct-token
count and at most 64 sorted `{token, occurrences}` entries. Scorecard consumes the evidence only
when both hashes are valid and both bounded multisets are complete; old snapshots and deliberately
truncated evidence retain the legacy reconstruction path. The evaluator never infers formula
boundaries from geometry.

CON-02 consumes the same version-1 TOML glossary used by the evaluator. Each source occurrence must
have the glossary's canonical target in its aligned translated paragraph. This makes multiple
renderings observable as missing canonical occurrences without guessing unlisted synonyms.

CON-01 and CON-02 are applicable to real output and the conserving fake profile. They are explicitly
`not-applicable` for legacy fake output, which intentionally discards the source tokens. CON-02 is
also `not-applicable` when no glossary is supplied. FOR-01 is a v2 mechanical proxy: it checks
delimiter balance and short numeric, closing-delimiter, underscore, or known `model` fragments
touching a model-labelled formula span. Once the formula repair round emits typed unit diagnostics,
the proxy must be upgraded to exact unit membership rather than tuned against this baseline.

FOR-04 closes the audit over all visible ink classes. MuPDF trace transforms are resolved to page
coordinates before comparison. A candidate must lie in the source formula neighborhood, fit the
formula footprint plus one source em, remain at the same input/output coordinates, and have at least
one source formula glyph leave that slot. Large figures and wider table/decorative rules are excluded;
typed paragraphs are source-preserving terminal states and are not violations. The full-artifact
audit records text, vector paths, and inline images separately; a text-only formula audit cannot
release an artifact.

FOR-05 independently checks the positive invariant that a published formula is one rigid body. It
reconstructs each source unit from formula glyphs plus uniquely associated vector/image ink, then
accepts the output only when every component is found after the same `(delta_x, delta_y)`. Candidate
glyph anchors are restricted to the unique matching `publication_ink.admissible_container`, expanded
by `0.5em` for source-glyph side bearings, so an identical formula elsewhere on the page cannot
satisfy the unit. Missing or duplicate publication evidence falls back to the source paragraph bounds
only for additive-IL compatibility; INK-01 independently rejects that incomplete ownership evidence.
FOR-04 catches ink abandoned at the source slot; FOR-05 catches a missing component or components
translated by different deltas even when the old slot is clean.
Both rules read the Typeset terminal state, so an ADR-0013 typed paragraph is excluded only after the
production pass has actually recorded its source-preserving reason; Translate IL alone is not a
publication-state oracle.

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
| INK-01 | final publication ink geometry | every nonidentity published paragraph has complete bounded evidence; exact glyph/origin multiplicity and owned vector/image ink exist in the final PDF; every component stays inside CropBox and its owning container; no cross-paragraph or retained-ink collision | critical per violation | Write IL `publication_ink` + pinned MuPDF trace |

LAY-05 is reserved because current debug IL does not expose final output line boxes. The comparator
ledger may compute it from pinned extractor XML; the harness must not invent it from string length.
For LNT-01, adjacent MuPDF `stext` fragments whose vertical intervals share a visual line and whose
horizontal gap is at most `1.5` times the larger fragment height are treated as one line. This keeps a
font switch inside one rendered line from turning legal internal punctuation into a false line-edge
violation; only the leftmost and rightmost fragments supply the visual line edges.

FOR-02 derives its bound per source paragraph. Source word-spacing samples are positive finite source
whitespace-character widths plus same-baseline `implicit_space_before` gaps whose characters on both
sides are `text/translate`; formula-adjacent gaps are excluded. The median is doubled and compared
with `1.5` times the paragraph median font size. A neighbor is required only where the source formula unit immediately touched a translatable
character on the same baseline. Formula-unit coverage uses the same evidence-backed extraction-order
normalization as production: a model-owned formula head/tail split by text that is geometrically
after the complete formula is audited as one unit. Source-unit adjacency uses metric boxes so natural
radical ink overhang does not masquerade as reversed source order; the final output gap retains the
strict lower bound. The output formula is matched by compact exact text and expected vertical
position; adjacent MuPDF glyph lines may jointly satisfy the complete exact text, and their bbox union
is measured against the nearest left/right extracted lines. Candidate neighbors share a visual line
when their vertical intervals overlap under the shared
`mimus-quality-contract::formula_items_share_line` rule; top-edge deltas are not a second threshold.
This prevents a compact superscript union from excluding visually overlapping `=` or formula-name
spans, and prevents a standalone radical on the same page from satisfying a complete `sqrt` unit.
Unmatched formula
units remain visible in `formula_units` versus `matched_units`; v2 does not invent geometry for them.
FOR-03 intentionally shares FOR-02's derived bound, matching the adjudicated single continuity
contract for fixed-slot and relocation paths.

FOR-04/FOR-05 attribute public MuPDF trace ink only when it satisfies the production-aligned
formula-cap or row-separator geometry and has exactly one geometric owner among the formula units in
the same paragraph. Broad neighborhood overlap is not ownership evidence: a rule below an adjacent
formula or an accent above a different visual line is excluded. The output anchor search includes a
`0.5em` ink extent around the unique Write-IL `admissible_container`; a candidate still passes only
when every source formula glyph and every uniquely owned path/image is present under one identical
delta. This permits an accepted `multi_line_bounds_expanded` plan to use its published owner instead
of the narrower source paragraph while retaining the page-wide and cross-paragraph INK-01 checks.

INK-01 closes the final-publication taxonomy independently of the planning implementation:

| Ink class | Production execution point | Public evidence and final observation | Container / obstacle rule | Legal terminal state |
| --- | --- | --- | --- | --- |
| translated output glyphs | `plan_text_segment` / `place_formula_flow` plan at or above 8 pt; `build_typeset_fonts` fixes the rounded `/W` advances before `install_typeset_replacements` | additive IL v1 `TranslatedText` components record bounded line summaries; every glyph records its exact final visual bounds, Unicode/baseline origin, and `font_slot` from the embedded subset face, and pinned MuPDF trace must match each declared glyph one-to-one | every component stays in resolved CropBox and the owning layout container, extended vertically only by accepted final ink; per-glyph ink, never whitespace inside a line summary, must not overlap another published paragraph or retained vector/image ink; instrumented CJK/Latin glyphs must use a slot permitted by the shared script classifier | publish with complete evidence, or preserve the whole source paragraph with the existing typed reason; identity and passthrough paragraphs require no evidence |
| source formula glyph replay | `source_formula_units` and `place_formula_flow` preserve source bytes/font identity and apply one page-space delta | `SourceTextReplay` records each source glyph's final visual bounds and exact Unicode/baseline origin under one nonzero ownership group; MuPDF trace must match every declared glyph one-to-one | same CropBox/container rules; per-glyph ink participates in collisions, while components in the same formula group may overlap as one rigid body; other groups and paragraphs may not | complete fixed/relocated group, or typed `typeset_protocol` / `typeset_overflow` source preservation |
| retained or replayed vector path | `paragraph_typeset_obstacles` blocks ordinary retained paths; `uniquely_owned_text_underlines` and `source_formula_units` alone may claim safe complete programs for replay | `VectorPath` records every replayed final bound (`group=0` for a text underline, nonzero for formula ownership); all other MuPDF trace paths remain retained obstacles | a replay must match trace at its declared bound; retained paths may not intersect published text; a nonzero vector group must have exactly one nonempty source-text owner | exact replay under the owner delta, or typed paragraph preservation; ambiguous path ownership is never evidence |
| raster / inline image | `walk` records both `Do` Image XObjects and inline images under active clips; `paragraph_typeset_obstacles` blocks both, while `source_formula_units` may claim only a uniquely owned self-contained inline-image program | `InlineImage` records replayed formula images; unmatched MuPDF `fill_image` records are retained raster ink | partial overlap with published text is forbidden; an image containing the entire admissible layout container is classified as an intentional background rather than an obstacle; same-group formula image overlap is allowed | exact same-group inline replay, nonoverlapping/background retention, or typed paragraph preservation; Image XObjects are retained obstacles and never relocation programs |

The oracle requires exactly one `publication_ink` entry for every paragraph whose Write IL is
non-preserved and whose translation differs from reconstructed source text. Evidence on identity,
passthrough, preserved, or unknown paragraphs is itself a violation. Component bounds must be finite
and nonempty; glyph ink bounds must be finite, non-inverted, and contained by their component summary.
Outline-free whitespace uses a zero-area box at its baseline and never participates in collision
tests. Unmatched MuPDF text is not by itself a violation because a translated paragraph can retain
passthrough source units; exactness means every declared output glyph is observed one-to-one at its
declared origin. Retained path and image geometry is intersected with every active MuPDF trace clip;
`clip_path` / `clip_image_mask` push a clip and `pop_clip` restores the prior state. The production
walker mirrors PDF `W` / `W*` semantics: the rule takes effect only when the current path terminates,
is saved and restored by `q` / `Q`, and follows Form scope isolation. In bilingual output the
translated observation page is `2 * page_index + 1`; default
output uses `page_index`. Trace coordinates are rebased from MuPDF's mediabox origin to the declared
CropBox origin before matching. The sole fixture observation exemption is
`unit-xobj-depth-overflow`: its adjudicated 65-Form input intentionally makes MuPDF trace terminate
with `exception stack overflow!`, so the gate still checks evidence completeness, CropBox/container,
formula ownership, and cross-paragraph geometry but records final trace observation as exempt. No
paper or other fixture inherits that exemption.

### 2.4.1 Detection/execution alignment ledger

Every FOR/CON detector must name the production action that enforces the same conservative contract,
or an explicit exemption. A detector-only critical rule is a release blocker, not evidence that the
pipeline handled the defect.

| Rule | Production execution point | Failure action / exemption |
| --- | --- | --- |
| COV-01 / RSK-01 Unicode reliability | `walk::font::read_simple_encoding` calls the single source `mimus-quality-contract::differences_agl_single_scalar` only for explicit `/Differences` names that the legacy decoder could not resolve; ParagraphFind writes additive IL `unicode_source=differences_agl` and emits typed `unicode_recovered` counts | unknown or multi-scalar names, implicit Standard/MacRoman entries, ToUnicode-unmapped codes, composite fallback, and any cross-engine weak conflict remain whole-paragraph typed `unreliable_unicode`; no retry or guessed scalar |
| COV-01 / COV-03 Form ownership | ParagraphFind compares visible upright character ownership with the page's direct `/Contents` set before Unicode reliability: Form-only `Translate` paragraphs are typed, mixed page/Form paragraphs remain processable, and documents with translation-eligible whole-page wrappers propagate the root cause to every paragraph on wrapper pages; scorecard keeps policy-eligible paragraphs in the COV-01 denominator and reads all typed reasons into COV-03 | preserve Form-only or whole-page-wrapper content as `form_xobject_content`, issue no request for Form bytes, and leave those bytes unchanged; a document containing only `Passthrough` chart Form text is outside this detector |
| CON-01 | `mimus-quality-contract::conserved_tokens` is called per formula-delimited semantic segment by `translate::executor::execute`; complete runtime token evidence feeds `scorecard::conservation_measurement` | one corrective translation retry; a second violation preserves the whole paragraph with typed `content_conservation` (introduced by the stacked T2 PR) |
| CON-02 | prompt construction injects the exact version-1 glossary; scorecard remains the independent aligned-output detector | documented exemption: glossary consistency is a semantic release proposal pending user approval, not a conservative runtime identity predicate; no production typed degradation is claimed |
| FOR-01 | `pass::complete_model_formula_boundaries`, placeholder restoration, formula byte/font identity replay, and output round-trip validation | ambiguous boundary or replay evidence becomes `typeset_protocol`; unplaceable complete units become `typeset_overflow`; the scorecard heuristic remains an independent proxy |
| FOR-02 | `pass::plan_paragraph` runs `normalize_formula_interleaved_punctuation_order` and `formula_continuity_is_valid` for fixed-slot and relocated plans; bound arithmetic and visual-line membership come only from `mimus-quality-contract::{formula_continuity_limit, formula_items_share_line}` | repair by evidence-based segment normalization/relocation; otherwise `typeset_overflow` |
| FOR-03 | same execution point and bound as FOR-02; it is the area projection of the same excessive gap, not a separate threshold | same repair/typed action as FOR-02 |
| FOR-04 | `pass::source_formula_units` attaches uniquely owned path/image spans to the relocation unit; `paragraph_plans_leave_orphan_source_ink` rejects unclaimed source-slot ink before installation | exact source programs replay under the glyph delta; ambiguous ownership or residual ink becomes typed `typeset_protocol` |
| FOR-05 | `pass::source_formula_units` uses whole-paragraph unique visual radical ownership and closes each unit over formula glyph/path/image components; `place_formula_flow` applies one delta to the resulting unit | missing/ambiguous ownership, unsafe replay, or a component that cannot share the unit delta becomes typed `typeset_protocol` |
| TRANS-01 | `pass::prepare_retained_section_number_prefix` identifies the source prefix/title geometry, and every ordinary, slotted, shared-operand, and formula-flow planner calls the single `mimus-quality-contract::retained_section_number_position` rule; Write IL publishes the source and output geometry for the independent scorecard | keep the source prefix x and source title x; if output-prefix width would leave less than `0.25em`, clamp to `0.25em` and emit `section_number_gap_clamped`; otherwise any missing/inconsistent evidence is a scorecard violation, while an unplaceable plan follows existing typed fitting/overflow paths |
| INK-01 | `walk` tracks painted path, inline-image, and `Do` Image XObject geometry under deferred `W` / `W*` clips and `q` / `Q` state; `pass::typeset` emits `publication_ink` only after incompatible plan components are removed and output fonts fix final glyph origins; planning already enforces CropBox, owning layout container, other-paragraph ink, clipped retained path/image obstacles, and ADR-0020 ownership | any candidate failure remains typed source-preserved before publication; the independent Write-IL + MuPDF oracle applies the trace clip stack and treats missing/mismatched evidence or final collision as a critical artifact violation; only the named `unit-xobj-depth-overflow` trace observation is exempt |

### 2.4.2 M3 adjudicated layout and typesetting policy ledger

| Case | Detector and boundary | Production action / terminal state |
| --- | --- | --- |
| FORM-07 | `StylesAndFormulas` accepts each recorded model `inline_formula` assignment as one complete span; comma and bracket syntax inside that span do not create subspans | request preparation emits exactly one placeholder for each recorded span and restores that same unit |
| ORDER-04 | `ParagraphFind` considers cross-column continuation only for one model `abstract` assignment whose lines form exactly two geometric columns with at least two lines each | flatten the left column top-to-bottom, then the right column top-to-bottom, into one paragraph; ordinary `text`, multiple model regions, fallback lines, and ambiguous column counts retain existing separation |
| TYPE-05 | `mimus-quality-contract::{forbidden_line_start, forbidden_line_end}` is the single minimal punctuation set used by both final-PDF LNT-01 measurement and the shared ordinary/slotted/formula-flow token stream; paragraph-leading closing and paragraph-trailing opening punctuation are unsatisfiable | join every forbidden break before placement, never hang ink beyond the container, and return existing typed `typeset_overflow` when no legal line placement fits at or above 8pt |
| TYPE-06 | output text is consumed exactly as returned; no script-transition detector adds spacing | glyph advances contain only output-font advances and explicit response whitespace; there is no automatic CJK/Latin gap. Restoring a retained section number's source-geometric title origin under TRANS-01 is position recovery, not inserted text spacing |
| TYPE-07 | `paragraph_typeset_obstacles` includes visible ink from every other paragraph and `ink_bounds_are_safe` applies the unchanged collision threshold to each independently planned paragraph | a plan that cannot avoid later paragraph ink at the 8 pt floor becomes typed `typeset_overflow`; no paragraph is moved |
| TYPE-09 | `StylesAndFormulas` requires every character in the natural paragraph to share model `text`, the complete source to match the conservative math shape, at least one strong operator, and at most two whitespace-delimited operand-like tokens | mark the whole source paragraph passthrough, emit informational `math_passthrough`, create no request or output-font resource, and do not count degradation; all other model prose remains translatable |
| FONT-10 | the Type0 walk proves reliable Unicode and PDF advance but `/CIDToGIDMap` resolves outside the embedded TrueType glyph count | retain the PDF advance; collision-check the transformed conservative font-level bbox union; set additive IL v1 `bbox_estimated`; emit informational `glyph_bbox_estimated`; all ordinary unreliable-font branches remain typed paragraph preservation |
| output-font variation | `OutputFontFaces::parse` and `build_embedded_font` share the ADR-0018 slot resolver: each weight slot prefers its exact named `Regular`/`Bold` instance, then clamped `wght=400/700`, then an empty location; every `ttf-parser` metric and `subsetter` outline uses that user-coordinate list | the planner's rounded 1/1000-em advance must equal the embedded CID `/W`; configured ink drives wrapping, collision, 8 pt, and CropBox gates; each embedded subset outline must match its configured source instance; a font that cannot be parsed or instantiated retains the existing typed `unsupported_font`/startup asset failure boundary; font identity remains absent from translation cache keys |
| output-font script routing | production and scorecard call the same `mimus-quality-contract::output_script_preference`: Han/CJK forms/kana/hangul and `U+2010-2027` prefer CJK; ASCII, Latin letters, Greek, Cyrillic, Letterlike Symbols, Mathematical Operators, Arrows, and Superscripts/Subscripts prefer STIX; other scalars prefer CJK | Latin preference is STIX Text weight -> STIX Math -> CJK weight; CJK/default preference is CJK weight -> STIX Text weight -> STIX Math. Bold selects the matching weight where one exists. All glyphs stay at one point size and baseline, while line ascent/descent use CJK Regular/Bold only. Write IL `font_slot` must be complete once present; `SourceTextReplay` must never claim an output slot. Historical IL without slots remains accepted |
| PARSE-06 | Parse checks every classic-xref normal entry against the objects lopdf actually parsed and retains the entry's object number, generation, and byte offset | reject as Input/2 `pdf_parse` with additive `detail.object_syntax`; publish no output; unrelated parse failures continue to omit detail |
| STREAM-03 | the operator walk requires exactly two finite numeric operands for each `m` and `l`; any short, excess, or nonnumeric path makes vector ink ownership unknowable | mark the page `graphics_unreliable`, emit typed page degradation and summary, keep page IL empty, and publish the original page bytes |
| PARA-05 | `ParagraphFind` considers an in-region natural boundary only when the whole region is uniformly model-backed, translatable `text`, on a real downward line step, the candidate first-line baseline origin is more than `1.2em` right of the lower-median line start, and the preceding line occupies no more than 80% of the model container width; every other label and mixed-policy region retains the established splitter | split at that line and create separate translation requests; indent alone, an underfilled predecessor alone, same-baseline prose fragments, sparse-column left outliers, formulas, charts, images, and passthrough categories retain their established composition |
| PARA-09 / TYPE-12 | for uniformly model-backed `text` that remains translatable after page-zero author protection, `ParagraphFind` subtracts the median continuation baseline-origin x from the first-line baseline-origin x; visual ink and side bearings do not participate; nonpositive deltas are omitted | store positive page-space points in additive IL v1 `first_line_indent`; every Typeset candidate starts the first output line at exactly `container.left + indent` and continuation lines at `container.left`; an obstacle at that exact start fails closed; fallback, footnote, formula, and protected-author shapes omit the field |
| FORM-12 | `uniquely_owned_text_underlines` accepts only one safe complete `q/Q` horizontal path, one complete translated text-show owner in the same content object, no characters outside the paragraph sharing its span, substantial below-baseline overlap, and one final single-line owner-to-output delta | exclude only that proven path from paragraph obstacles, claim its source span, replay it under the owner's exact page-space delta, and include the relocated path in safety/output bounds; any suspicious unsafe, incomplete, cross-object, multiply owned, multiline, or indeterminate case becomes typed `typeset_protocol` with the source paragraph and underline intact |
| FORM-14 | `preferred_body_font_size` counts visible, non-whitespace, translatable characters by exact page-space font size after excluding geometrically proven super/subscripts; the character-weighted mode wins and exact ties choose the larger size | when proven script characters outnumber ordinary body characters, every fixed and obstacle-aware planner uses that single selected point size before the existing 8pt fallback sequence; otherwise the established mean-based start remains stable |

Test-level alignment checklist:

| Contract | Automated assertion | Full-artifact audit |
| --- | --- | --- |
| explicit Differences + AGL recovery | `CMAP-10` Type1 and `CMAP-11` Type3 public CLI/debug-IL fixtures; quality-contract acceptance/rejection table tests; implicit StandardEncoding, ToUnicode-unmapped and weak-conflict preservation tests | reconcile every `unicode_recovered` paragraph/count against the producer × font × ToUnicode × encoding bucket ledger; independently inspect five dense rendered pages |
| continuity bound | `mimus-quality-contract` worked examples plus production and scorecard source-sampling tests | report each paragraph bound and its source samples |
| fixed + relocated formula order/adjacency | `formula_continuity_oracle_rejects_extraction_order_text_after_following_formula`, punctuation normalization, atomic-chain and fixed-to-relocation tests | audit every formula paragraph for unit order, neighbor gap and inline hole |
| formula glyph/unit completeness | boundary fixtures plus formula byte/font replay and round-trip tests | dual-extractor glyph inventory, script baseline and unit membership |
| formula ink closure | generated composite-formula and nearby-table-rule fixtures; unsafe graphics scope fails closed | every formula paragraph audits text, vector path and inline-image ownership; FOR-04 must be zero unexplained |
| formula rigid-body delta | displaced-radical production regressions plus scorecard `detached_source_order_radical_breaks_formula_rigid_body_integrity` | every published formula component shares one delta; FOR-05 must be zero unexplained |
| numeric/unit/reference conservation | shared lexer tests plus loopback retry/preserve tests in the stacked T2 PR | CON-01 must be 100%; every residual is typed or a blocking defect |

Items that depend on visual or bilingual judgment remain explicit report rows; a named regression
paragraph is only a sample and never substitutes for the full formula audit.

### 2.5 Typesetting lint (`typesetting_lint`)

| ID | Subclass | Formula | Severity | Source |
| --- | --- | --- | --- | --- |
| LNT-01 | Chinese kinsoku | final output line starts with closing punctuation or ends with opening punctuation | major | pinned MuPDF final-PDF text lines, with translated-text fallback when extraction is unavailable |
| LNT-02 | isolated punctuation | one-scalar punctuation paragraph | minor | translated text |
| LNT-03 | placeholder residue | `{v`, `{l`, or `<b` token prefix remaining | critical | translated text + extracted PDF text |
| LNT-04 | abnormal whitespace | leading/trailing or doubled ASCII whitespace | minor | translated text |
| LNT-05 | English residue | suspicious echo count; lexical ratio is reserved until language segmentation is pinned | minor | summary |
| TRANS-01 | retained section-number positioning | for every source heading with a retained `Number` prefix, the output title-left delta is at most `0.1em`; a wider output prefix may instead use the shared `0.25em` clamp only with matching typed info | critical | ParagraphFind + Write IL `publication_ink.section_number_gap` + NDJSON diagnostics |

### 2.6 Structural fidelity (`structural_fidelity`)

| ID | Subclass | Formula | Severity | Source |
| --- | --- | --- | --- | --- |
| STR-01 | valid container and page count | qpdf input/output checks pass and page counts match | critical per failure | pinned qpdf |
| STR-02 | incremental-write prefix | output bytes begin with every input byte | critical | raw bytes |
| STR-03 | non-text object inventory | recursive qpdf JSON counts of Form, Image, Link, Annot, Outlines match | critical | pinned qpdf JSON |
| STR-04 | masked non-text pixels | identical RGB pixels / compared pixels after masking translated bboxes plus 2 pt | critical; `10 * (1-fidelity)*1000` | pinned Poppler PPM at 72 DPI |
| STR-05 | title/complete-author-block conservation | page-0 title and every geometrically selected `text` paragraph in the title/author band are policy passthrough and write-IL identical, with canonical source/write hashes | critical per failed binary invariant | ParagraphFind + Write IL |

The pixel check intentionally excludes text replacement footprints. It is sensitive to antialiasing
and renderer versions, so comparisons must pin `pdftoppm`; it complements rather than replaces the
object and span checks.

STR-05 is applicable only when both page-0 anchors exist. It reports four binary invariants: title
policy, complete visual author-block policy, title identity, and author-block identity. Identity
includes Unicode, source code, font, font size, baseline, metric box, visual bbox, passthrough
payload, and absence of a non-identity translation. Canonical SHA-256 values bind the complete
selected source and Write blocks as evidence without adding another weighted invariant. Missing
anchors are fail-closed to `not-applicable`, not a guessed author range.

The shared geometric definition ignores paragraph index and model reading order. The bottom edge of
the page-0 `doc_title` paragraph is the upper anchor. The nearest geometrically lower `abstract` or
`paragraph_title` paragraph supplies its top edge as the lower anchor. The band extends by half the
median positive font size across both anchors on each side, a line-height-scale tolerance reported
as `band_tolerance`; only textual `text` or `fallback_line` paragraphs whose full bounds lie inside the resulting
`band_lower`/`band_upper` interval belong to the author block. Production passthrough and STR-05
call the same `mimus-quality-contract::title_author_band` function. A missing/reversed anchor or
missing anchor font-size evidence remains not applicable rather than widening the band.

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
- 100% CON-01 conservation and zero unexplained FOR-01/FOR-04/FOR-05 formula violations;
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
| Chinese title/body weight | Before M3.7, the Noto Sans SC VF defaulted to Thin for Regular; M3.7 pins `wght=400/700` and makes Noto Serif SC the default | typesetting style, minor | resolved by #113 |
| abstract position | heading source `(283.758,386.532)-(328.243,397.280)`, mimus `(283.758,382.826)-(307.668,397.172)`, BabelDOC `(283.866,384.739)-(307.776,401.906)`; Chinese width change is expected, vertical shifts are measurable | LAY-01, minor candidate | retained in baseline; no new issue pending broader sample |
| page 13 attention heading | mimus and BabelDOC left/right/bottom match; top differs 0.48 pt at 150 DPI; figure and rotated labels remain intact | LAY-01, minor | accepted render quantization, no defect |

Under schema v1, the anchor measured 99.17% paragraph coverage (119/120), 100% Han-weighted coverage (6,936/6,936),
one typed `unreliable_unicode`, one suspicious echo, six expansions, zero placeholder residue, and
99.9801% masked pixel fidelity. Its score does not erase the three human-confirmed defects above.

## 7. Recovery-round result

The 2026-08-31 conserving-fake refresh replaces the historical 2,997/23-bucket planning snapshot for
this implementation round: current master publishes 20/20 with 5,030 `unreliable_unicode`
paragraphs across 16 TeX papers and zero in the four Word papers. Splitting formula from
text/numeric content yields 59 populated buckets. The strict
explicit-Differences/AGL-single-scalar candidate scope is 1,945/5,030 paragraphs (38.7%): 1,128
pdfTeX Type1 text/numeric, 155 pdfTeX Type1 formula, 376 LuaTeX Type1 text/numeric, 107 LuaTeX
Type1 formula, 52 XeTeX Type1 text/numeric, 68 XeTeX Type1 formula, and 59 Type3.

After the conservative decoder, `unreliable_unicode` falls from 5,030 to 3,149 (-1,881, 37.4%).
Within the 1,945 candidate paragraphs, 1,878 become fully translated, 65 remain typed because the
unchanged weak cross-engine conflict gate disagrees with the AGL result, and two split into a safe
translated subparagraph plus a typed unknown subparagraph. Formula and text/numeric recovery remain
separate: 308 formula paragraphs and 1,570 text/numeric paragraphs become fully translated. The
after-change IL records 7,971 recovered characters, while unresolved portions retain typed
`unreliable_unicode`.

Seven excluded mixed buckets also split at existing paragraph boundaries into a safely decoded
subparagraph and a still-typed unknown subparagraph. This is not policy expansion: implicit
Standard/MacRoman paths, ToUnicode-unmapped or multiscalar codes, unknown/multiscalar Differences,
composite fallback, and weak conflicts are never decoded by the new rule. The 20-paper replay stays
20/20 published with `Internal/6 = 0`, CON-01 missing occurrences 0, FOR-04/FOR-05 0, and no new
unexplained FOR-01...05 finding.

## 8. Adjudication log

| Date | Decision | Consequence |
| --- | --- | --- |
| 2026-08-29 | Reverse the earlier title judgment: `doc_title` and the complete visual author block must be typed passthrough. LLM identity is not a policy contract. | A translated title is now a STR-05 failure. Production implementation remains in the separate formula/title repair stack. |
| 2026-08-29 | Formula-boundary leakage (`value ls]`) is a human-confirmed critical content defect. Fixed-slot and relocated formula continuity share one oracle. | FOR-01 is critical; FOR-02 and FOR-03 use one derived bound. The 1706 conclusion is blocked even if the numeric total is high. |
| 2026-08-29 | Human-confirmed critical defects override automatic totals. | Reports retain the numeric score for diagnosis but cannot conclude release eligibility while `confirmed_criticals` is non-empty. |
| 2026-08-30 | A real `Internal/6` is always a production bug. | Cluster rows remain failed/N/A; they may not be relabelled as typed degradation or assigned fabricated scores. |
| 2026-08-30 | During the formula/title stack rebase, schema-v1 verdict and STR-06 were classified as duplicates of schema-v2 conclusion and STR-05. The old identifiers are not retained. | STR-05 absorbs only the unique dual-box identity and canonical source/write hash evidence, with its four existing weighted invariants unchanged. |
| 2026-08-31 | Formula auditing is ink-closed. FOR-04 detects formula ink left at a vacated source slot; FOR-05 detects missing components or components that do not share one rigid-body delta, including when the old slot is clean. | Text-only formula audits cannot release an artifact. Every published formula must satisfy both rules or have an ADR-0013 typed terminal reason. |
