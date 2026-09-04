# M3 scorecard v2 baseline

Date: 2026-08-30, refreshed 2026-09-03. Schema: scorecard v2. Thresholds remain proposals pending
user approval. The 20-paper cluster rows in Sections 1-3 preserve an `invalid-profile` historical
run and are not comparable with a conserving baseline. Sections 4-8 retain separate real-anchor,
process, acceptance, and pre-M3.7 history. Section 9 is the current Noto Serif SC 2.001 baseline.

## 1. Historical evidence and conclusion (cluster `invalid-profile`)

> **Invalid profile:** the archived 20-paper outputs in Sections 1-3 were labelled conserving, but
> their Translate IL is the compressed legacy fake profile. These numbers are retained for audit
> history only and must not be used as a baseline or compared numerically with Section 8.

The anchor is the archived real Chinese L5-4R output for `1706.03762v7`; its source, IL, NDJSON,
and output SHA-256 were checked as one consistent artifact set. The other rows were archived under
the loopback conserving-fake label, which Section 8.1 later disproves. No paper was downloaded and
no real translation API was called.

The historical 20-paper publication rate is 18/20 (90%); `Internal/6` is 2/20 (10%). These values
and the historical typed-degradation median of 293 and worst value of 1,278 describe the
`invalid-profile` run only. They do not establish conserving-fake behavior.

The real anchor's automatic total is 93.142025, but its conclusion is
`blocked_by_confirmed_critical`: formula-boundary leakage produced `value ls]`, and title plus the
complete author block violate the adjudicated passthrough policy. Human-confirmed critical defects
override the numeric total without rewriting it.

## 2. Historical per-paper matrix (`invalid-profile`)

`Con` is CON-01 conservation; `Formula`, `Gap`, and `Hole` are violation counts; `T/A` is failed
STR-05 title/author invariants. Legacy v1 and v2 totals are not directly comparable because the
formula set and applicability profiles changed.

| Paper | Producer | v1 | v2 | Status | Typed | Con | Formula | Gap | Hole | T/A |
| --- | --- | ---: | ---: | --- | ---: | ---: | ---: | ---: | ---: | ---: |
| 1706 L5-4R | anchor | N/A | 93.142025 | critical-blocked | 1 | 0.921788 | 33 | 8 | 8 | 4 |
| 01 Adam | pdfTeX | 93.912835 | 89.506705 | published | 353 | 1.000000 | 70 | 25 | 25 | 0 |
| 02 ResNet | pdfTeX | 95.800920 | 95.229025 | published | 362 | 1.000000 | 7 | 2 | 2 | 0 |
| 03 SqueezeNet | pdfTeX | 95.838364 | 94.611010 | published | 135 | 1.000000 | 19 | 2 | 2 | 0 |
| 04 MobileNets | pdfTeX | 95.748645 | 94.935964 | published | 276 | 1.000000 | 3 | 0 | 0 | 0 |
| 05 BERT | pdfTeX | 94.893724 | 94.596316 | published | 258 | 1.000000 | 5 | 0 | 0 | 0 |
| 06 DDPM | pdfTeX | 95.903850 | 93.478314 | published | 410 | 1.000000 | 56 | 9 | 9 | 0 |
| 07 ViT | pdfTeX | 96.727054 | 95.995907 | published | 308 | 1.000000 | 9 | 5 | 5 | 0 |
| 08 LoRA | pdfTeX | 96.347250 | 94.595559 | published | 451 | 1.000000 | 53 | 13 | 13 | 0 |
| 09 Repliable onion routing | XeTeX | 90.847210 | 88.943065 | published | 1,278 | 1.000000 | 275 | 57 | 57 | 0 |
| 10 Compact IBE | XeTeX | 95.792900 | 92.600261 | published | 144 | 1.000000 | 91 | 11 | 11 | 0 |
| 11 SDitH hardware | XeTeX | 96.810611 | 96.079369 | published | 281 | 1.000000 | 66 | 3 | 3 | 0 |
| 12 Hertz side channel | XeTeX | 95.202583 | 94.109479 | published | 102 | 1.000000 | 89 | 6 | 6 | 0 |
| 13 Information-theoretic MPC | LuaTeX | 97.167447 | 94.880177 | published | 174 | 1.000000 | 49 | 34 | 34 | 0 |
| 14 Masked comparisons | LuaTeX | 95.521412 | 94.385500 | published | 586 | 1.000000 | 32 | 7 | 7 | 0 |
| 15 LWE two-step | LuaTeX | 94.670453 | 91.686829 | published | 305 | 1.000000 | 151 | 12 | 12 | 0 |
| 16 Supersingular orientations | LuaTeX | 92.100472 | 80.526476 | published | 544 | 1.000000 | 408 | 14 | 14 | 0 |
| 17 Informational consciousness | Word | 83.397208 | N/A | Internal/6 `output_mismatch` | N/A | N/A | N/A | N/A | N/A | N/A |
| 18 Consciousness model | Word | 79.634868 | 93.746733 | published | 124 | 1.000000 | 38 | 22 | 22 | 0 |
| 19 Multibeam IoT | Word | 67.108472 | N/A | Internal/6 `output_mismatch` | N/A | N/A | N/A | N/A | N/A | N/A |
| 20 Tuberculosis biosensor | Word | 90.833849 | 95.221220 | published | 32 | 1.000000 | 1 | 3 | 3 | 0 |

Word 18 originally appeared to preserve only 298/423 conservation tokens. That was an evaluator
false positive: Form XObject characters labelled `translate` in IL were never eligible for the
production request. Matching production's visible/upright/policy/direct-`/Contents` contract yields
287/287 on the fresh run.

## 3. Historical cluster and reproducibility (`invalid-profile`)

| Producer | Papers | Published | Internal | Publication | Internal rate | Typed median | Typed worst |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| pdfTeX | 8 | 8 | 0 | 100% | 0% | 330.5 | 451 |
| XeTeX | 4 | 4 | 0 | 100% | 0% | 212.5 | 1,278 |
| LuaTeX | 4 | 4 | 0 | 100% | 0% | 424.5 | 586 |
| Word | 4 | 2 | 2 | 50% | 50% | 78.0 | 124 |

The failed rows preserve their exact terminal evidence:

- 17 Informational consciousness: output page 2 is missing typeset U+6A21 at
  `(52.733277, 692.056800)`.
- 19 Multibeam IoT: output page 9 is missing typeset U+6A21 at
  `(54.460650, 439.751160)`.

Both reproducibility samples are byte-identical. ResNet produced
`81fb385d6a2512019883a9ea0b9fecdb777c8b89809f7e6fd0fa34b55ef149a9` twice. Repliable onion
routing produced `813bcf95b5aa65b8cadafd48c6534bf1e42d535ca8c2c46a95a7b5d9123bea4a` twice. The
second runs intentionally omit debug artifacts and hash the published PDF; this avoids making disk
capacity part of the reproducibility oracle.

## 4. 1706 semantic QE review pack

The sidecar scores 117 aligned, non-preserved real translation paragraphs on CPU. Model:
`Unbabel/wmt20-comet-qe-da`, revision
`2e7ffc84fb67d99cf92506611766463bb9230cfb`; snapshot-tree SHA-256
`f1358ea1c226134157fdf29f7481b9ff71b95b6183428aed0d4feed93bfea70d`; checkpoint SHA-256
`05d892bf4a3e34b9a4de239109387d43107b2a8c55ad34b73a929ca6c1ede24e`. Distribution: minimum
-0.737445, p10 -0.439157, median -0.016397. QE is a review signal and is not part of the six-dimension
total.

| Page/order | Score | Source | Translation |
| --- | ---: | --- | --- |
| 5/15 | -0.737445 | Position-wise Feed-Forward Networks | 逐位置前馈网络 |
| 5/20 | -0.732226 | Embeddings and Softmax | 嵌入和 Softmax |
| 5/8 | -0.664882 | and W ∈ . | 且 W ∈。 |
| 5/6 | -0.566660 | Where the projections are parameter matrices i i i | 其中投影是参数矩阵iii |
| 7/10 | -0.561399 | − We used the Adam optimizer [20] with 1 2 and ϵ = 10 . We varied the learning rate over the course of training, according to the formula: | − 我们使用了 Adam 优化器 [20]，其12以及 ϵ = 10 。 |
| 4/3 | -0.528134 | Scaled Dot-Product Attention | 缩放点积注意力 |
| 8/50 | -0.509723 | Label Smoothing During training, we employed label smoothing of value ls ]. This hurts perplexity, as the model learns to be more unsure, but improves accuracy and BLEU score. | 标签平滑 在训练期间，我们采用了值为ls]的标签平滑。这会损害困惑度，因为模型学会变得更加不确定，但会提高准确率和 BLEU 分数。 |
| 3/6 | -0.508569 | An attention function can be described as mapping a query and a set of key-value pairs to an output, where the query, keys, values, and output are all vectors. The output is computed as a weighted sum | 注意力函数可以描述为将一个查询和一组键-值对映射为一个输出，其中查询、键、值和输出都是向量。输出计算为加权和 |
| 4/10 | -0.493972 | Multi-Head Attention | 多头注意力 |
| 3/2 | -0.486809 | Encoder and Decoder Stacks | 编码器和解码器堆栈 |

## 5. Limitations and access ledger

Per-page timing is N/A because public protocol/debug artifacts expose only stage and page progress,
not stable per-page durations. Follow-up #133 records that deferred PRO-04 work. Cache hit rate is
also N/A for the `--no-cache` conserving-fake sweep. No semantic score is assigned to fake output.

Authorized public access during this task was limited to:

1. PyPI dependency resolution for the pinned Python environment with `uv pip compile`.
2. Hash-locked installation from `tools/quality-eval/requirements.lock.txt`.
3. One failed anonymous request for gated `Unbabel/wmt22-cometkiwi-da` (HTTP 401).
4. One successful download of public `Unbabel/wmt20-comet-qe-da` at the revision and hashes above.
5. Authorized GitHub issue, comment, push, and PR operations.

The final QE rerun used `HF_HUB_OFFLINE=1` against that verified cache. Cargo ran only with
`--locked --offline`. The scorecard adds the existing workspace `toml` crate for version-1 glossary
parsing; no new Cargo package or network fetch was needed. No key, paper byte, model, or Python
environment is committed.

## 6. Historical next-round note

First repair the two new #115 `Internal/6` regressions; real input must publish or end in an
ADR-0013 typed degradation. After that, the data favors #118 before broad #38 fixture backfill:
#118's existing producer/font/ToUnicode/encoding matrix has a conservative recovery estimate of
1,639/2,997 paragraphs (54.7%), while #38 remains at 1/46 M3 fixture coverage. Rerun this exact
schema after the recovery round; if realized recovery is materially below the estimate, use the
bucket residuals to choose the next fixtures rather than filling all gaps indiscriminately.

## 7. L5-5R2 withdrawal and replacement baseline

The 2026-08-30 acceptance remains withdrawn. Its `98.067441` automatic score and text-only formula audit
did not cover vector paths or inline images. The 2026-08-31 FOR-04 replay finds six source-slot vector
residues; `(3,9)` is confirmed critical because `sqrt(d_k)` moved while its fraction and radical rules
stayed behind and numerator `1` was cleared. `(4,21)` was reopened for ink-closed review. These rows
remain historical evidence and do not release the withdrawn PDF.

| Contract | Pre-fix baseline | Withdrawn L5-5R2 | Disposition |
| --- | ---: | ---: | --- |
| CON-01 | 92.1788% | **162/162 (100%)** | runtime retry then typed `content_conservation` uses the scorecard lexer |
| FOR-01 proxy | 33 | **6** | all six explained by full-artifact audit; zero unexplained |
| FOR-02 excessive gap | 8 | **0** | production and scorecard share bound and visual-line membership |
| FOR-03 unexplained hole | 8 | **0** | same execution contract as FOR-02 |
| STR-05 title/author failures | 4 | **0** | structure-owned typed passthrough |
| Confirmed criticals | formula leakage + title/author | **none** | automatic conclusion is non-blocking |
| FOR-04 orphan source ink | not measured | **6** | blocking; text-only PASS withdrawn |

The full formula population is 54 paragraphs: 52 published and two typed (`(3,12)`
`unreliable_unicode`, `(4,6)` `typeset_overflow`). The replay proves all 307 exclusive formula
operand spans, all three shared spans/seven shared glyphs, all source font references, and all 468
formula characters. FOR-02/FOR-03 measure 64 neighbor gaps with a maximum of 9 pt and no violation.
Eighteen noncontiguous extractor records preserve the same source extraction-order shape; they are
not glyph loss.

The final 20-paper conserving-fake regression publishes 20/20 with Internal/6 = 0, zero degraded
pages, zero `content_conservation_retry`, and zero `content_conservation` typed reason. Preserved
paragraph counts are workload characteristics, not publication failures:

| Producer | Published | Internal/6 | Papers' preserved-paragraph range |
| --- | ---: | ---: | ---: |
| pdfTeX | 8/8 | 0 | 121-480 |
| XeTeX | 4/4 | 0 | 86-1,223 |
| LuaTeX | 4/4 | 0 | 126-536 |
| Word | 4/4 | 0 | 21-287 |

The runtime conservation net changes #118's expected behavior: a wild paragraph whose translation
drops a conservatively detectable number, unit, or bracketed reference now receives one corrective
retry, then publishes the original paragraph under typed `content_conservation` instead of caching
or typesetting damaged text. Recovery estimates for the 2,997-paragraph matrix must therefore report
three outcomes per bucket: translated recovery, typed conservation fallback, and other typed residue.
The existing 54.7% recovery estimate remains the reason to run #118 before the broad #38 fixture
backfill, but it must be remeasured under this fail-closed split.

### 7.1 Ink-closed replacement (accepted 2026-08-31)

The replacement artifact is `.context/vector-formula-fix/real6/1706.03762v7.zh.pdf`, SHA-256
`b3de6f10522f64a7e8bedba292c01d51724fb616f298bd4917ed8e54a475c0ef`. Its score is
`97.988578`, conclusion `automatic_score_only`, with no confirmed critical. The lower automatic
total than the withdrawn artifact is not a regression verdict; the replacement adds FOR-05 and uses
the ink-closed audit while retaining six independently explained FOR-01 proxy records.

| Contract | Replacement | Disposition |
| --- | ---: | --- |
| CON-01 | **161/161 (100%)** | zero missing numeric/unit/reference occurrence |
| FOR-01 proxy | **6** | all six individually explained; zero unexplained violation |
| FOR-02 / FOR-03 | **0 / 0** | 61 measured neighbor gaps, maximum 9 pt |
| FOR-04 orphan source ink | **0/71** | published units only; typed source-preserving rows are excluded |
| FOR-05 rigid-body integrity | **0/4** | detached radical/vector units use one page-space delta |
| STR-05 title/author failures | **0** | structure-owned typed passthrough |
| Confirmed criticals | **none** | scorecard conclusion is non-blocking |

The full population remains 54 formula paragraphs: 52 published and two source-preserving typed
rows, `(3,12)` `unreliable_unicode` and `(4,6)` `typeset_overflow`. The audit explicitly records the
screenshot regression `(3,4)` as complete `sqrt(d_k)`, plus complete rigid bodies at `(3,9)` and
`(4,21)`. The accepted ordinary and strict replays both hit 137/137 cache entries and make zero
provider calls; strict exits 4 for exactly those two reviewed typed paragraphs and publishes no PDF.

## 8. Corrected conserving baseline (accepted 2026-08-31)

This section supersedes the cluster claims in Sections 1-6. It measures the Round A stack tip using
the `conserving_translation` loopback profile with a fake key and no cache, then replays the real
anchor from the immutable archived cache against a confirmed-closed endpoint. The run made zero real
translation API calls, downloaded no papers or models, and wrote no paper-derived text to the
repository.

### 8.1 Why the archived baseline is invalid

The #118 archive contains `t0-conserving-runs`, `t3-conserving-runs`, and `t3-final-runs`. Their
NDJSON names the model `m3-118-conserving-fake-v1`, but the artifact bytes prove that the loopback
served the compressed legacy profile:

- t0 used loopback port 57929 while t3 used 58574, yet all three Word-17 Translate IL files are
  byte-identical with SHA-256
  `1150317812f85d77d858c1363f061b746a112c6985254fb2762aeb4e0040a0f1`;
- the same representative paragraph has 103 source characters and only 33 translated characters in
  all three directories;
- `t3-conserving-runs` and `t3-final-runs` are therefore duplicate mislabeled output, not independent
  conserving evidence.

The archive and its scores remain read-only historical evidence. The exact server implementation
that produced the bytes was not retained, so the bounded conclusion is a mislabeled loopback
behavior or archival copy, not a production regression and not a reason to reconstruct the old run.

### 8.2 Corrected 20-paper matrix

`v2` is the automatic scorecard v2 total. `Typed` is the terminal typed preserved-paragraph count.
`Con` is CON-01 conservation. `Formula`, `Gap`, and `Hole` are the FOR-01 proxy, FOR-02, and FOR-03
violation counts; `Ink` and `Rigid` are FOR-04 and FOR-05; `T/A` is failed STR-05 title/author
invariants. Fake output cannot establish semantic translation quality, so these totals are mechanical
regression measurements only.

| Paper | Producer | v2 | Typed | Con | Formula | Gap | Hole | Ink | Rigid | T/A |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 01 Adam | pdfTeX | 93.318966 | 152 | 1.000000 | 88 | 0 | 0 | 0 | 0 | 0 |
| 02 ResNet | pdfTeX | 98.384383 | 20 | 1.000000 | 8 | 0 | 0 | 0 | 0 | 0 |
| 03 SqueezeNet | pdfTeX | 95.769723 | 20 | 1.000000 | 11 | 0 | 0 | 0 | 0 | 0 |
| 04 MobileNets | pdfTeX | 98.496032 | 4 | 1.000000 | 5 | 1 | 1 | 0 | 0 | 0 |
| 05 BERT | pdfTeX | 97.285482 | 5 | 1.000000 | 3 | 1 | 1 | 0 | 0 | 0 |
| 06 DDPM | pdfTeX | 95.740492 | 85 | 1.000000 | 48 | 0 | 0 | 0 | 0 | 0 |
| 07 ViT | pdfTeX | 99.108393 | 8 | 1.000000 | 2 | 3 | 3 | 0 | 0 | 0 |
| 08 LoRA | pdfTeX | 96.335498 | 25 | 1.000000 | 39 | 0 | 0 | 0 | 0 | 0 |
| 09 Repliable onion routing | XeTeX | 87.232674 | 1,113 | 1.000000 | 274 | 17 | 17 | 0 | 0 | 0 |
| 10 Compact IBE | XeTeX | 91.911922 | 107 | 1.000000 | 89 | 1 | 1 | 2 | 0 | 0 |
| 11 SDitH hardware | XeTeX | 95.521806 | 72 | 1.000000 | 63 | 1 | 1 | 0 | 0 | 0 |
| 12 Hertz side channel | XeTeX | 90.666845 | 11 | 1.000000 | 84 | 11 | 11 | 0 | 0 | 0 |
| 13 Information-theoretic MPC | LuaTeX | 94.535178 | 27 | 1.000000 | 38 | 24 | 24 | 0 | 0 | 0 |
| 14 Masked comparisons | LuaTeX | 95.910949 | 43 | 1.000000 | 39 | 0 | 0 | 0 | 0 | 0 |
| 15 LWE two-step | LuaTeX | 90.144038 | 128 | 1.000000 | 127 | 6 | 6 | 0 | 0 | 0 |
| 16 Supersingular orientations | LuaTeX | 83.764106 | 175 | 1.000000 | 178 | 5 | 5 | 0 | 0 | 0 |
| 17 Informational consciousness | Word | 95.678227 | 20 | 1.000000 | 0 | 0 | 0 | 0 | 0 | 0 |
| 18 Consciousness model | Word | 91.367401 | 69 | 1.000000 | 16 | 19 | 19 | 0 | 0 | 0 |
| 19 Multibeam IoT | Word | 90.713475 | 48 | 1.000000 | 2 | 1 | 1 | 0 | 0 | 0 |
| 20 Tuberculosis biosensor | Word | 96.716659 | 0 | 1.000000 | 1 | 2 | 2 | 0 | 0 | 0 |

All 20 papers publish and `Internal/6` is zero. The cluster typed median is 35 and the worst is
1,113. Producer strata are:

| Producer | Papers | Published | Internal/6 | Typed median | Typed worst |
| --- | ---: | ---: | ---: | ---: | ---: |
| pdfTeX | 8 | 8/8 | 0 | 20.0 | 152 |
| XeTeX | 4 | 4/4 | 0 | 89.5 | 1,113 |
| LuaTeX | 4 | 4/4 | 0 | 85.5 | 175 |
| Word | 4 | 4/4 | 0 | 34.0 | 69 |

The primary loopback run accepted 3,275 conserving translation responses and zero term-extraction
calls. Every checked request names `m3-118-conserving-fake-v1`; no real provider or paper download
was used. Aggregate CON-01 is **7,161/7,161**, STR-05 has zero failures, and FOR-02/FOR-03 report
92/92 continuity findings without a new unexplained regression.

FOR-04 is 2/2,597: both rows are the same two paper-10 vector paths present in the base, policy, and
final scorecards. They predate #150 and remain attributed accepted-baseline findings. FOR-05 is
0/55. Its denominator and the paper-06 inherited findings changed because the final scorecard now
requires production-aligned cap/row geometry and a unique formula owner instead of assigning every
nearby trace path to every formula. Paper 15's path belongs to the visual line above; paper 16's two
neighboring paths belong to other formula units, and its real overbar moved under the exact glyph
delta. The regression tests preserve both the unique-owner and paragraph-edge anchor cases.

Among the 18 papers with a comparable pre-#150 published artifact, Adam remains byte-identical and
the other 17 hashes change because the configured variable-font Bold instance now affects both
planning metrics and embedded outlines. Paper 15 and paper 16 publish only after the final retained-
character repair. Their final SHA-256 values are
`1af7633e922d2012fe06f8dfa039a2fbfcc48f7dd9b63ddabeff07d33f5505a2` and
`57f31f736e0bea9a35886d79d7a2a61303ccd70ffb4a11ccf678993961d3e3c4`.

### 8.3 Current closed-cache anchor

The accepted input cache had SHA-256
`e5e825564ff2166c672db271c48745b1e467057ab8be09d51f4adca14f58e94c` before replay. The endpoint
`http://127.0.0.1:9/v1` was confirmed closed. All 108 paragraph requests were cache hits; there were
zero misses, zero retries, zero errors, and zero provider calls in each mode. This is the one baseline
update after TYPE-05, FORM-12, and #150: the previous accepted default SHA was
`b3de6f10522f64a7e8bedba292c01d51724fb616f298bd4917ed8e54a475c0ef`; the combined TYPE-05 /
FORM-12 policy candidate was `44fc1ee63e0adbf350e9985f83c8bef905c9384a194f2669fe6dc5133eaf0e16`
but was not adopted as a separate baseline; the final default and `--bilingual` SHA-256 values are:

- default: `1e34692b54c52306c1cadcd4aad3a7c01ceae6a30a1b0787021b681100266622`;
- bilingual: `5f2c6cd3d8ee36d81c0c6582cacc4cfec3f7b91f6cc5e8df32235af308333743`.

| Measurement | Current anchor |
| --- | ---: |
| scorecard v2 total | 97.646525 |
| conclusion | `automatic_score_only` |
| typed rows | `(3,12) unreliable_unicode`; `(4,6) typeset_overflow` |
| CON-01 | 161/161 |
| FOR-01 proxy | 6 |
| FOR-02 / FOR-03 | 0 / 0 |
| FOR-04 orphan source ink | 0/71 |
| FOR-05 rigid-body integrity | 0/4 |
| STR-05 title/author failures | 0 |
| page count default / bilingual | 15 / 30 |

`qpdf --check` passes for both modes. The default anchor has 62 measured formula-neighbor gaps with
maximum 2 pt and no unexplained inline hole. Its score and PDF bytes change because the final policy
and variable-font layers are active; the cache key remains font-independent, as proved by the same
108 immutable-cache hits.

## 9. M3.7 Noto Serif SC baseline (accepted 2026-09-03)

The user selected Noto Serif SC 2.001 as the production default. The asset is the variable TTF at
noto-cjk commit `523d033d6cb47f4a80c58a35753646f5c3608a78`, path
`Serif/Variable/TTF/Subset/NotoSerifSC-VF.ttf`, size 25,139,544 bytes, SHA-256
`69467baf421bdbb32b292d6c092ed033ca32e5f7a0d06194e69901287b50b2f3`, and cache directory
`fonts/noto-serif-sc-2.001/`. It was fetched once from the corresponding raw GitHub URL. Noto Sans
SC remains selectable through `--font` and `--font-bold`.

Regular and Bold resolve the exact named instances when present, otherwise clamped `wght=400/700`.
For `U+4E00`, the full variable-font default ExtraLight bounds are `[53,401,953,505]`, the resolved
Regular bounds are `[47,397,958,514]`, and the embedded Regular subset bounds are
`[48,398,959,514]`. `pdffonts` reports `NotoSerifSC-Regular` and `NotoSerifSC-Bold` for the final BERT
artifact.

### 9.1 Closed-cache BERT replay

The immutable M3.6 cache SHA-256 is
`7def75b43ed17ab3b909e152f6abdffacdfa28d4f52f104c384714e58fef2c5d`. The adjudicated dev-only
migration copied its unique `extracted_glossaries_v1` value to the new production-computed key in a
writable cache copy. The deliverable `05-bert-m3-7-author-geometry.redb` has SHA-256
`e9b9c11d25a8ba2ed91b3c02961c88820f0e6aa58bdbf4fc889cf697b039a797`; its provenance sidecar has
SHA-256 `9727e547e5393c5231de70678d94206ca2599e3e1954c132551e9702a7f79648` and records
`model_calls: 0`.

The final replay used default cache-resolved fonts, model `m35-proxy-model`, auto terms enabled, and
the confirmed-closed endpoint `http://127.0.0.1:9/v1`. It published with 197 cache hits, zero misses,
zero transport failures, and zero model calls. Page-zero reading orders 11 and 12 remain source
identity as `Jacob Devlin Ming-Wei Chang Kenton Lee Kristina Toutanova` and `Google AI Language`.
The full Translate IL is byte-identical to the adjudication-validated migrated replay; only those two
requests were removed relative to M3.6. The final PDF SHA-256 is
`bb485d9ba02760934b5a5412f76f6a8b7b0ea2025d0cce0bcd281a559b5989fa`.

| Measurement | Final BERT |
| --- | ---: |
| scorecard v2 total | 97.502090 |
| conclusion | `automatic_score_only` |
| typed rows | 14 |
| CON-01 | 380/380 |
| FOR-01 proxy | 3 |
| FOR-02 / FOR-03 | 1 / 1 |
| FOR-04 orphan source ink | 0/21 |
| FOR-05 rigid-body integrity | 0/0 |
| INK-01 | 0/181 publications; 870 components |
| STR-05 title/author failures | 0 |

### 9.2 Final 20-paper conserving-fake matrix

`Ink` is FOR-04 and `Rigid` is FOR-05. The matrix comes from the accepted run after FOR-05 began
using the unique Write-IL `admissible_container`; scores from the earlier false-positive run are not
the baseline.

| Paper | Producer | v2 | Typed | Con | Formula | Gap | Hole | Ink | Rigid | T/A |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 01 Adam | pdfTeX | 90.886015 | 556 | 1.000000 | 80 | 0 | 0 | 0 | 0 | 0 |
| 02 ResNet | pdfTeX | 98.308487 | 22 | 1.000000 | 8 | 0 | 0 | 0 | 0 | 0 |
| 03 SqueezeNet | pdfTeX | 94.674860 | 40 | 1.000000 | 11 | 0 | 0 | 0 | 0 | 0 |
| 04 MobileNets | pdfTeX | 98.483401 | 4 | 1.000000 | 5 | 1 | 1 | 0 | 0 | 0 |
| 05 BERT | pdfTeX | 97.186985 | 8 | 1.000000 | 3 | 1 | 1 | 0 | 0 | 0 |
| 06 DDPM | pdfTeX | 95.734756 | 85 | 1.000000 | 48 | 0 | 0 | 0 | 0 | 0 |
| 07 ViT | pdfTeX | 98.873281 | 25 | 1.000000 | 2 | 0 | 0 | 0 | 0 | 0 |
| 08 LoRA | pdfTeX | 96.345861 | 26 | 1.000000 | 39 | 0 | 0 | 0 | 0 | 0 |
| 09 Repliable onion routing | XeTeX | 87.242840 | 1,114 | 1.000000 | 274 | 16 | 16 | 0 | 0 | 0 |
| 10 Compact IBE | XeTeX | 91.915119 | 106 | 1.000000 | 89 | 1 | 1 | 2 | 0 | 0 |
| 11 SDitH hardware | XeTeX | 95.520313 | 72 | 1.000000 | 63 | 1 | 1 | 0 | 0 | 0 |
| 12 Hertz side channel | XeTeX | 90.206650 | 35 | 1.000000 | 84 | 11 | 11 | 0 | 0 | 0 |
| 13 Information-theoretic MPC | LuaTeX | 94.883171 | 32 | 1.000000 | 38 | 18 | 18 | 0 | 0 | 0 |
| 14 Masked comparisons | LuaTeX | 95.905461 | 44 | 1.000000 | 39 | 0 | 0 | 0 | 0 | 0 |
| 15 LWE two-step | LuaTeX | 90.109823 | 126 | 1.000000 | 127 | 5 | 5 | 0 | 0 | 0 |
| 16 Supersingular orientations | LuaTeX | 83.776234 | 173 | 1.000000 | 178 | 5 | 5 | 0 | 0 | 0 |
| 17 Informational consciousness | Word | 95.457662 | 24 | 1.000000 | 0 | 0 | 0 | 0 | 0 | 0 |
| 18 Consciousness model | Word | 91.149963 | 85 | 1.000000 | 16 | 18 | 18 | 0 | 0 | 0 |
| 19 Multibeam IoT | Word | 90.772033 | 50 | 1.000000 | 2 | 1 | 1 | 0 | 0 | 0 |
| 20 Tuberculosis biosensor | Word | 96.832673 | 0 | 1.000000 | 1 | 2 | 2 | 0 | 0 | 0 |

All 20 papers publish and `Internal/6` is zero. The cluster typed median is 42 and the worst is
1,114. Producer strata are:

| Producer | Papers | Published | Internal/6 | Typed median | Typed worst |
| --- | ---: | ---: | ---: | ---: | ---: |
| pdfTeX | 8 | 8/8 | 0 | 25.5 | 556 |
| XeTeX | 4 | 4/4 | 0 | 89.0 | 1,114 |
| LuaTeX | 4 | 4/4 | 0 | 85.0 | 173 |
| Word | 4 | 4/4 | 0 | 37.0 | 85 |

The primary run accepted 3,251 conserving responses; the two deterministic reruns bring the log to
3,663 requests. Every request names `m3-118-conserving-fake-v1`, no term-extraction request was made,
and no real provider was used. Aggregate CON-01 is **7,151/7,151**. FOR-04 remains the same two
accepted paper-10 findings, now 2/2,418 with no new row. FOR-05 is 0/46, INK-01 is zero across 2,972
published paragraphs and 9,803 components, and STR-05 has zero failures. FOR-02/FOR-03 report 80/80
mechanical findings.

The accepted run has 160 `typeset_overflow` rows. It removes seven from the initial Serif run and
adds none; relative to the Sans control it removes eight and adds seven, for one fewer in aggregate.
The 8 pt floor, collision, CropBox, FOR-04/FOR-05, and INK-01 contracts are unchanged. ResNet is
byte-identical across reruns at
`4d21d06d90c1a4cf70e2412727d7c1b9fc53e57a85229d3acf9f9272722a7dcf`; Repliable onion routing is
byte-identical at `6dbf6f14262d5120b0b9bd77d065d34b930cfea0b326783f45127bfaa2597870`.

The FOR-05 correction is an evaluator ownership fix, not a production relaxation. Supersingular
orientations page 5/order 4 moved the complete formula and its fraction rule by approximately
`(-10.2071, -10.2849) pt`; its unchanged output SHA-256 is
`f802f8e34cfb18f68948caa2e275830eb2943835d81c6393e1114a123b8c4032`. The source paragraph plus
`0.5em` window was narrower than the accepted `multi_line_bounds_expanded` owner, so the old
scorecard reported one false FOR-05 violation. The unique published `admissible_container` removes
that false positive while independent CON, FOR-04, INK-01, and STR-05 results remain unchanged.

### 9.3 Re-anchored real replay

The accepted immutable cache remains
`e5e825564ff2166c672db271c48745b1e467057ab8be09d51f4adca14f58e94c`. With the archived 96-entry
glossary (`abc661f7ab8a80209e05adccf3cbf56418cf710a9fb0eddebe8945c9c001705a`) and the closed endpoint,
default and bilingual modes each hit 108 entries with zero misses, transport failures, retries, or
provider calls. The new outputs are:

- default: `eea884d23484ff6a1336cc0c1c1c1ada60bfc593173ccc442dec75ef0e9e2ab7`;
- bilingual: `1901f9ce2fecc7fb524dd7c9051f9aca18dc4afa631d9f0f846c82918331912a`.

The default score is 97.626297 with the same two typed rows, CON-01 161/161, FOR-01 6,
FOR-02/FOR-03 0/0, FOR-04 0/71, FOR-05 0/4, INK-01 zero across 107 publications and 348 components,
and STR-05 zero. Default and bilingual page counts are 15 and 30; `qpdf --check` passes both.

The out-of-repository page-one abstract comparison is
`.context/m3-7/comparison/m3-7-final-compare-abstract.png`, SHA-256
`9cbbe2dcf3eb50fd56491ccb6f9ad0f1ae02aad65036878a05ab58469fe03cb9`. Its left-to-right panels are
the old Thin default, explicit Sans 400, final Serif output, and BabelDOC Source Han Serif reference,
all rendered at 150 DPI over the same crop.
