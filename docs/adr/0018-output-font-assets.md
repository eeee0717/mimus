# ADR-0018: Output font asset resolution

- Status: Accepted (2026-08-26), amended for the two-family stack (2026-09-04)
- Decision level: Public and difficult to reverse because CLI flags, environment names, config
  keys, cache layout, and downloaded asset identities are user-facing contracts.

## Context

The first M2 production Typeset path compiled the nine-code-point `corpus/fonts/MimusCJK.ttf`
fixture into `mimus-core`. Real Chinese translations therefore passed Translate and were then
preserved as `unsupported_font`; the translated output contained no Han characters. This also
violated ADR-0005, which requires large output fonts to remain runtime assets rather than binary
payloads.

Tests need deterministic CJK coverage without contacting a public endpoint. Input fixture fonts,
test output fonts, and production output assets consequently need distinct ownership and paths.

## Decision

1. Typeset receives CJK Regular/Bold, Latin Regular/Bold, and same-family symbol output-font bytes
   through `PassContext`. Production code does not use `include_bytes!` for output fonts and does
   not read or download fonts inside Typeset.
2. Each CJK font slot resolves at CLI startup in this order: `--font` / `--font-bold`,
   `MIMUS_FONT_REGULAR` / `MIMUS_FONT_BOLD`, `font_regular` / `font_bold` in the user config,
   a SHA-256-validated cache entry, then a manifest download. The Latin slots use canonical
   `--font-latin` / `--font-latin-bold`, `MIMUS_FONT_LATIN` / `MIMUS_FONT_LATIN_BOLD`, and
   `font_latin` / `font_latin_bold` before the same cache and manifest stages. The former
   `--font-fallback*`, `MIMUS_FONT_FALLBACK_*`, and `font_fallback_*` names remain deprecated aliases;
   canonical environment names win when both are set, while canonical and deprecated CLI flags
   conflict. `MIMUS_CACHE_DIR` or `cache_dir` selects the cache root. Otherwise the platform user
   cache directory is used.
3. `--asset-mirror`, `MIMUS_ASSET_MIRROR`, or `asset_mirror` replaces the manifest base URL. Mirror
   values must be credential-free HTTP(S) base URLs without a query or fragment. Downloads are
   bounded to 64 MiB, checked against the pinned SHA-256, and published atomically.
4. A missing, unreadable, malformed, oversized, hash-mismatched, or non-instantiable font fails
   before PDFium and the pipeline with `Asset/output_font_unavailable` (exit 3). Custom CJK slots
   must cover the CJK sample; custom Latin slots must cover the ASCII, Latin, and Greek sample.
   `configuration_resolved` exposes every canonical source and SHA-256 as additive CLI v2 fields and
   retains the prior fallback fields with values identical to their Latin counterparts.
5. Glyph selection is character-level and style-preserving. The shared script classifier routes:

   - Han, CJK punctuation (`U+3000-303F`), fullwidth forms (`U+FF00-FFEF`), CJK compatibility,
     kana, hangul, and `U+2010-2027` to CJK Regular/Bold first;
   - ASCII (`U+0020-007E`), Latin-1 and Latin Extended letters, Greek, Cyrillic, Letterlike Symbols,
     Mathematical Operators, Arrows, and Superscripts/Subscripts to STIX Two Text Regular/Bold first;
   - every other scalar to CJK first.

   A Latin-first miss tries STIX Two Math and then CJK. A CJK/default-first miss tries STIX Two Text
   and then STIX Two Math. The selected font supplies wrapping metrics, ink bounds, subsetting, PDF
   resource selection, extraction mapping, and additive IL `font_slot` provenance for that character.
   Line ascent/descent come only from the CJK Regular/Bold slots; all selected glyphs use the same
   baseline and point size without scaling. Only used slots are embedded, so one output contains at
   most the two decided families and five logical resources. A character missing from all three
   choices preserves only the affected paragraph as `unsupported_font` and emits
   `unsupported_output_glyph` with page, paragraph reading order, a unique missing-character sample,
   and all exact font identities. Structural
   paragraph mismatches use `typeset_protocol`; an absent unambiguous content span uses
   `unlocatable`. Every selected glyph advance is rounded to the nearest integer 1/1000 em before
   wrapping and placement, and the same value is written to the CID font `/W` array. This keeps
   layout geometry and extractor coordinates identical across fonts with different units per em.
6. `corpus/fonts/MimusCJK*.ttf` remain input fixture fonts only. Output-font tests inject the
   deterministic GB2312 level-one-scale assets under `crates/mimus/tests/assets/fonts/` through the
   public path/config seam. Those files include their generation recipe, pinned hashes, and OFL,
   and are never linked into a production target.
7. Each logical weight slot resolves one variation location before any font query. An exact named
   `Regular` or `Bold` instance wins for the corresponding slot; otherwise a `wght` axis receives
   `400` or `700`, clamped to that axis's user-space bounds. Static fonts and variable fonts without
   a matching instance or `wght` axis use an empty coordinate list. The same user coordinates
   configure `ttf-parser` before every advance, bounding-box, ascender, descender, wrapping,
   collision, and fitting query, and instantiate the subsetter's outlines and metrics. The embedded
   `/BaseFont` uses the named instance's fvar `postScriptNameID` when present, otherwise the family
   name plus logical slot. Output-font identities and coordinates do not enter translation or
   terminology cache keys. This replaces the prior Regular default-location/byte-compatibility
   rule: on 2026-09-03 the production Noto VF default was proven to be `wght=100`, which had made
   all ordinary Chinese body text Thin instead of Regular.

## Production manifest

The CJK slots use the pinned Noto Serif SC 2.001 variable subset from noto-cjk commit
`523d033d6cb47f4a80c58a35753646f5c3608a78`, path
`Serif/Variable/TTF/Subset/NotoSerifSC-VF.ttf`, SHA-256
`69467baf421bdbb32b292d6c092ed033ca32e5f7a0d06194e69901287b50b2f3`, and cache directory
`fonts/noto-serif-sc-2.001/`. Both logical weights map to that one variable font and cache entry.
Regular resolves its named `wght=400` instance while Bold resolves its named `wght=700` instance.
The resulting static PDF subsets therefore carry distinct, metric-consistent outlines without
duplicating the downloaded asset. Noto Serif SC is the default because its serif construction
matches the Times-like body typography common in academic papers. Noto Sans SC remains available
through `--font` and `--font-bold`; this changes neither resolution precedence nor cache-key
semantics.

The Latin Regular/Bold slots use the TrueType-outline STIX Two Text 2.13 b171 variable font from
stipub/stixfonts tag `v2.13b171`, commit `744a22a4dd626cd14d75728aef34fc8ad7c85db0`, path
`fonts/variable_ttf/STIXTwoText[wght].ttf`, size 418,956 bytes, SHA-256
`7962b8b7811e6a896c9a91a0bccbb5241047770eb24d4997c5cb5fe21d5c0df2`, and cache directory
`fonts/stix-two-text-2.13b171/`. Named `Regular` and `Bold` instances resolve to
`STIXTwoText-Regular` and `STIXTwoText-Bold`. The google/fonts copy at commit
`9017368e541f77a66e2302f474d2142d1bb77f5c` is byte-identical, but the manifest uses the upstream
STIX repository according to the recorded source priority.

STIX Two Text covers the audited `U+0141 LATIN CAPITAL LETTER L WITH STROKE` and
`U+03F5 GREEK LUNATE EPSILON SYMBOL`, but its cmap does not contain `U+2217 ASTERISK OPERATOR`.
The authorized same-family symbol slot therefore uses TrueType-outline STIX Two Math 2.12 b168a
from the same google/fonts commit, path `ofl/stixtwomath/STIXTwoMath-Regular.ttf`, size 1,517,976
bytes, SHA-256 `562551b15b836e6e01d1b7350909baf3c8c8d83260c1190fbf4544333e6936de`, and cache directory
`fonts/stix-two-math-2.12b168a/`. This replaces the former sans-serif fallback and cancels the prior
follow-up to replace that fallback with static assets. M4 still owns `assets pull`, model assets,
progress UX, and the unified public manifest surface; it must consume this M3.9 manifest unchanged.

## Consequences

- The production binary no longer contains the tiny corpus fixture font or any other output font.
- Offline users can provide both families' four weight slots explicitly; normal startup downloads
  and caches verified Text/Math assets on first use. A custom Latin family also supplies its symbol
  slot so an offline override does not silently mix a third family.
- A real translation can lose only paragraphs whose CJK, Latin, and symbol slots all lack required
  glyphs, and the diagnostic identifies the missing sample and every exact font identity.
- CI remains offline: download behavior is exercised only against a loopback HTTP server, then the
  same assets are resolved from cache after that server stops.
- Variable-font style no longer splits planning from publication: `/W`, extractor positions,
  wrapping, the 8 pt floor, CropBox checks, and collision checks all observe the instantiated slot.
