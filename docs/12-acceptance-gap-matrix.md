# M3.5 acceptance gap matrix

This matrix records what M3.5 actually exercised at `53a5c23`, what the evidence can support, and
what must remain open for M4 release acceptance. It is not a claim that a generated fixture proves
behavior on a comparable real-world PDF. Paper bytes, rendered comparisons, request logs, and caches
remain outside git under `.context/m3-5/`; aggregate evidence is attached to issue #173.

## 1. Evidence rules

| Mark | Meaning |
| --- | --- |
| PASS | The asserted behavior passed with the stated evidence class. |
| OBSERVED | A real artifact was produced, but the cell is descriptive rather than a release gate. |
| PARTIAL | A useful path ran, but material combinations or adjudication are still missing. |
| BLOCKED | The attempted path could not produce admissible evidence within its safety/reconciliation boundary. |
| GAP | No admissible run exists. |
| N/A | The combination is intentionally inapplicable. |

Evidence classes are `S` (generated fixture/unit or integration test), `O` (archived real PDF with a
fake, identity, parse-only, or skip-translation backend), `R` (authorized real translation backend),
and `A` (immutable accepted-cache replay). `S` never silently upgrades to `O` or `R`. Cross-tool text,
geometry, overlap, and formula-word measurements are review proxies, not semantic or INK-01 verdicts.

The offline corpus is 20 qpdf-clean papers plus the 1706 anchor: pdfTeX 9 including the anchor,
XeTeX 4, LuaTeX 4, and Word 4, with 7-33 pages, single/double columns, formulae, tables, figures, and
some non-Latin metadata. It does not contain a real scan, a publisher-encrypted input, a native-CJK
paper, a beamer deck, or a genuinely large document.

## 2. CLI and workflow matrix

The fake/identity column includes the 21-paper sweep where meaningful; narrowly scoped generated
fixtures are identified as `S`. The real-backend column refers only to the four selected comparison
papers and must not be generalized across every producer.

| Surface | Fake / offline evidence | Real-backend evidence | Producer/input coverage | Status and remaining gap |
| --- | --- | --- | --- | --- |
| top-level `--help`, `--version` | CLI smoke | N/A | binary only | PASS (S); archive smoke remains #40/#42. |
| top-level `--json` | machine-protocol integration tests | inherited by translation runs | generated + selected papers | PASS (S/R where a run completes). |
| `inspect INPUT` | scan, encryption, malformed, rotation, debug and real-paper prefix runs | N/A: read-only | generated + all four producer layers | PASS (S/O). |
| `inspect --json` | typed terminal/error and diagnostic tests | N/A | generated + real papers | PASS (S/O). |
| `inspect --debug NEW_DIR` | pass-order/snapshot integration tests | N/A | generated | PASS (S); clean-machine filesystem path remains #42. |
| `inspect --layout-model`, `--layout` | pinned ONNX sweep; single-line tests | N/A | all layers for ONNX | PARTIAL: no real-paper cross-product with `single-line`. |
| `inspect --asset-mirror` | loopback SHA/reuse tests | N/A | generated asset server | PASS (S); public mirror journey remains #40/#42. |
| `translate INPUT`, `--output` | 21/21 conserving-fake publications | selected comparison set | all offline layers; selected R set | PASS (O); R results are reported per paper and not a corpus-wide gate. |
| `translate --json` | typed stages, errors, cache and degradation tests; 21-paper ledgers | selected runs capture NDJSON | generated + all layers | PASS where published; typed ledger is a comparative advantage over BabelDOC prose logs. |
| `--backend none` | configuration and source-preserving integration tests | N/A | generated | PASS (S). |
| `--backend openai`, `--endpoint`, `--model`, `--target-language` | loopback Responses service; URL/status/security matrix | same guarded proxy/model, `en -> zh-CN` comparison | four selected papers | PARTIAL: `zh-CN` only; no second language/backend. |
| `--font`, `--font-bold`, `--font-fallback`, `--font-fallback-bold` | pinned Noto/DejaVu 21-paper sweep and font tests | pinned files on selected runs | all layers | PASS for the pinned set; no broad user-font matrix. |
| `--layout-model`, `--layout` | production ONNX on 21 papers; synthetic single-line paths | production ONNX only | all layers for ONNX | PARTIAL: real backend did not cross with `single-line`. |
| `--asset-mirror` | loopback download, checksum, cache reuse | explicit local assets avoid network | generated only | PARTIAL: release package/first-run asset journey is #40/#42. |
| default automatic terms | second offline sweep includes one empty fake term call per eligible paper | Multibeam and Hertz completed with guarded exact-retry coalescing; BERT published no PDF | selected papers | PARTIAL: harness mitigation enabled two runs, but fixed 30 s production timeouts blocked BERT and remain #175. |
| `--no-auto-terms` | primary 21-paper sweep and anchor | cache/glossary replay modes | all layers + anchor | PASS (O/A); first sweep is not timing-fair with BabelDOC default terms. |
| `--glossary` | canonical TOML round-trip and precedence tests | BERT export/replay was attempted but no default PDF or admissible replay was published | generated + BERT | BLOCKED (R); BERT timeout ceiling prevented the real round trip. |
| `--dump-glossary` | stable dump tests | BERT automatic-term run was attempted but did not complete the publication workflow | generated + BERT | BLOCKED (R); no completed real dump/reuse evidence. |
| `--cache` | hit/miss/key/invalidation/retry/disconnect/security matrix | accepted anchor: 108/108 hits in default and bilingual; new real caches archived read-only immediately | generated + anchor + selected papers | PASS for mechanics/A; kill/resume and incomplete-real-cache behavior remain #174. |
| `--no-cache` | 21-paper timing sweep and exact request enumeration | BabelDOC comparison is cache-disabled; mimus enumeration only | all offline layers | PASS (O); no repeated real no-cache quality run. |
| `--concurrency` | validation tests and sweeps at 4 | selected runs at 4 | all layers at one value | PARTIAL: no 1/N scaling or race matrix on real papers. |
| `--strict` | synthetic degradation/status tests; 401 and Adam enumeration | no quality run is allowed to substitute strict for default | generated + selected Adam offline | PASS for policy (S/O); real provider degradation strict run remains a gap. |
| `--translate-table` | experimental flag tests only | not enabled | generated | GAP for real papers and semantic table review; keep experimental. |
| `--strip-link-borders` | annotation-scoped generated regression passes | Word 19 cache replay published a 12-page qpdf-clean PDF with one additional call | generated + Word 19 | PASS (S/R); the two source links already had zero-width borders, so the real replay correctly produced no annotation delta. |
| `--bilingual` | navigation regression; immutable anchor exact 30-page SHA replay | selected BERT workflow was attempted but no default cache-complete PDF existed | generated + anchor + BERT | BLOCKED for BERT (R); anchor PASS (A). |
| `--debug` | pass snapshots and scorecard contract | selected default runs create scorecard inputs | generated + selected papers | PASS where the default run completes; no disk-pressure run. |
| `assets pull` | command absent; help exposes only `translate` and `inspect` | N/A | N/A | GAP by design for M4, owned by #40/#41/#42. |
| BabelDOC latest stable default | 21/21 identity, skip and parse publications | BERT, Adam, and Multibeam published; Hertz exceeded its reconciled call ceiling without a PDF | all offline layers; selected R set | OBSERVED for 3/4 and BLOCKED for Hertz; no mimus-equivalent typed degradation or publication-ink ledger. |

## 3. Input-space matrix

| Input class | Current evidence | mimus result | BabelDOC comparison | Status / schedule |
| --- | --- | --- | --- | --- |
| native pdfTeX | 9 offline including anchor; BERT/Adam selected for R | 21-paper prefix/structure and selected quality evidence | identity/skip/parse and selected R | PASS (O), PARTIAL (R adjudication). |
| native XeTeX/xdvipdfmx | 4 offline; Hertz selected for R | Hertz published 24 qpdf-clean pages with 21 typed-preserved paragraphs | offline modes pass; BabelDOC R run exceeded its call ceiling without a PDF | PASS (O/R) for mimus publication; BLOCKED (R) for BabelDOC and PARTIAL pending human adjudication. |
| native LuaTeX/LuaHBTeX | 4 offline | qpdf-clean publications with typed ledger | all offline modes | PASS (O); no R sample. |
| Word/PDFMaker | 4 offline; Multibeam selected for R | qpdf-clean publications; weakest baseline stratum | all offline modes; selected R | PASS (O), PARTIAL (R adjudication). |
| real scan | none | generated image/OCR scan rejection and continuation matrix passes | generated image-only probe logged `contains no paragraphs`, returned 0, and published no PDF | GAP before #42: #174; BabelDOC result is S-only. |
| encrypted | generated RC4 empty-password guard and AES/non-empty rejection | typed `encrypted_pdf`, no output | generated AES probe logged `closed or encrypted`, returned 0, and published no PDF | PASS (S), GAP (real): #174. |
| legal rotated page | generated 90-degree page passes without degradation | visual transform and output checks pass | generated 90-degree BabelDOC probe published a one-page qpdf-clean PDF | PASS (S), GAP (real): #174. |
| malformed rotation (45 degrees) | generated negative control | typed rejection/degradation path | not a release input | PASS (S). |
| native CJK source | only generated predefined-CMap/overflow cases and non-Latin metadata | CJK overflow preserves source; decoding tests pass | generated predefined-CMap probe logged `too many CID chars`, returned 0, and published no PDF | GAP before #42: #174; BabelDOC result is S-only. |
| large document | maximum current input is 33 pages | no memory/disk scaling boundary established | same | GAP before #42: #174. |
| beamer / landscape | none real | no release-facing result | none | GAP before #42: #174. |
| dense formula | Adam, Hertz, LuaTeX papers, anchor | Hertz published 202/222 eligible paragraphs; Adam is fully preserved as 152 typed `unreliable_unicode` paragraphs | offline and selected R; BabelDOC Adam passes and Hertz is blocked | PARTIAL: Adam is not a mimus semantic-quality sample; formula/visual verdict remains human and recovery remains #143-#146. |
| tables / figures / links / outlines | corpus and generated annotation fixtures | incremental prefix preserves source inventory 21/21 | BabelDOC rewrites all 21; exact annotation/link inventory only 5/21 | PASS (O) for measured inventory; visual review still required. |

## 4. Failure and recovery matrix

| Failure | Evidence | Expected/observed behavior | Status / gap |
| --- | --- | --- | --- |
| wrong API key / HTTP 401 | loopback comparison | mimus default publishes source-preserving output with typed `translation_failure`; strict exits 4 with no PDF. BabelDOC retries twice then publishes source-preserving output with prose-only exception/fallback. | PASS (S comparison). |
| 429 and retryable 5xx | retry/status integration matrix | bounded retry, typed reason, strict/default policy | PASS (S); real service exhaustion remains #174. |
| malformed provider response / placeholder mismatch / content loss | translation security and conservation tests | correction is bounded; residual failure preserves whole paragraph with typed reason | PASS (S). |
| mid-run disconnect | loopback disconnect tests | source-preserving typed degradation; cache consistency checked | PASS (S); controlled real disconnect/reconnect remains #174. |
| fixed client timeout / late HTTP 200 | BERT and Hertz real automatic-term/paragraph calls | BERT remained blocked; a credential-free exact-retry coalescer let Hertz finish without bypassing the guarded proxy, but this is harness-only behavior | BLOCKED for production (R); #175 before #42 unless explicitly adjudicated. |
| interruption then cache resume | cache unit/integration tests only | keying and hit behavior pass | GAP for real process kill/resume: #174. |
| cache corruption/invalidation/security | generated cache matrix | mismatches do not become hits; sensitive material absent from keys/logs | PASS (S). |
| output/cache permission error | IO tests | typed IO failure and no false success | PASS (S). |
| actual ENOSPC | none | not established | GAP before #42: #174. |
| malformed/encrypted/scanned input | generated matrices plus BabelDOC scan/AES probes | mimus has typed terminal behavior and no accidental output; BabelDOC logged errors but returned 0 with no PDF for scan and AES | PASS (S) for mimus policy; comparative exit/output behavior OBSERVED; real representatives remain #174. |
| strict degradation | synthetic page/paragraph failures, 401, Adam offline | exit 4 and no publication | PASS (S/O); no successful real-provider strict quality run. |
| silent content loss | 21-paper dual extraction + qpdf audit | all modes publish qpdf-clean outputs; retention/proxy deltas are measured, not adjudicated as translation correctness | OBSERVED (O); human review required for R outputs. |

## 5. Release-gate carry-forward

Before #42 can close, #174 should provide license-safe real representatives and destructive fault
runs for scan, publisher encryption, rotation/landscape/beamer, native CJK, large files, ENOSPC,
kill/resume, disconnect/reconnect, and 429/5xx exhaustion. #175 owns the fixed provider timeout and
late-success retry amplification. #40 must define the actual archive footprint
and bundled dynamic-library/licenses; the current mimus footprint is only a measured approximation.
#41/#42 own the absent `assets pull`/skill journey.

The clean-machine pack should retain these already-pinned regressions:

- the four producer layers and exact SHA-256 manifest from the archived 20-paper corpus;
- BERT (mainstream layout), Adam (all-typed Unicode limitation), Multibeam (Word/links), Hertz
  (XeTeX/formulae), and 1706 (immutable default+bilingual cache replay);
- scan rejection/continuation, encryption, legal rotation, bilingual navigation, link-border scope,
  glossary round-trip, CJK overflow, cache/retry/disconnect/security, and 401 default/strict fixtures;
- `qpdf --check`, page count, source byte-prefix, object inventory, typed degradation, call ledger,
  cache ledger, secret scan, and empty human review sheets as mandatory evidence outputs.

Final semantic translation quality, terminology quality, and visual appeal remain user adjudications.
COMETKiwi, English residue, conserved-token, bbox, overlap, and out-of-bounds values can prioritize
pages for review; they cannot make that decision automatically.
