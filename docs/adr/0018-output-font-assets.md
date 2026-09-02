# ADR-0018: Output font asset resolution

- Status: Accepted (2026-08-26)
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

1. Typeset receives primary and fallback Regular and Bold output font bytes through `PassContext`.
   Production code does not use `include_bytes!` for output fonts and does not read or download
   fonts inside Typeset.
2. Each primary font slot resolves at CLI startup in this order: `--font` / `--font-bold`,
   `MIMUS_FONT_REGULAR` / `MIMUS_FONT_BOLD`, `font_regular` / `font_bold` in the user config,
   a SHA-256-validated cache entry, then a manifest download. The corresponding fallback slots use
   `--font-fallback` / `--font-fallback-bold`, `MIMUS_FONT_FALLBACK_REGULAR` /
   `MIMUS_FONT_FALLBACK_BOLD`, and `font_fallback_regular` / `font_fallback_bold` before the same
   cache and manifest stages. `MIMUS_CACHE_DIR` or `cache_dir` selects the cache root. Otherwise the
   platform user cache directory is used.
3. `--asset-mirror`, `MIMUS_ASSET_MIRROR`, or `asset_mirror` replaces the manifest base URL. Mirror
   values must be credential-free HTTP(S) base URLs without a query or fragment. Downloads are
   bounded to 64 MiB, checked against the pinned SHA-256, and published atomically.
4. A missing, unreadable, malformed, oversized, or hash-mismatched font fails before PDFium and the
   pipeline with `Asset/output_font_unavailable` (exit 3). The hint names all four font escape
   hatches. `configuration_resolved` exposes each selected source and SHA-256 as additive CLI v2
   fields.
5. Glyph selection is character-level and style-preserving: primary Regular or Bold is checked
   first, then the matching fallback weight. The selected font supplies wrapping metrics, ink
   bounds, subsetting, PDF resource selection, and extraction mapping for that character. Only
   slots used by the document are embedded, so one output contains at most two families and four
   weight resources. A character missing from both matching slots preserves only the affected
   paragraph as `unsupported_font` and emits `unsupported_output_glyph` with page, paragraph
   reading order, a unique missing-character sample, and both exact font identities. Structural
   paragraph mismatches use `typeset_protocol`; an absent unambiguous content span uses
   `unlocatable`. Every selected glyph advance is rounded to the nearest integer 1/1000 em before
   wrapping and placement, and the same value is written to the CID font `/W` array. This keeps
   layout geometry and extractor coordinates identical for fonts such as DejaVu Sans whose native
   units-per-em is 2048.
6. `corpus/fonts/MimusCJK*.ttf` remain input fixture fonts only. Output-font tests inject the
   deterministic GB2312 level-one-scale assets under `crates/mimus/tests/assets/fonts/` through the
   public path/config seam. Those files include their generation recipe, pinned hashes, and OFL,
   and are never linked into a production target.
7. The logical Regular slot keeps the font's default variation location and legacy subset path so
   existing body-text output remains byte-compatible. A logical Bold slot resolves one variation
   location before any font query: an exact named `Bold` instance wins; otherwise a `wght` axis
   receives `700`, clamped to that axis's user-space bounds. Static fonts and variable fonts without
   a Bold location use an empty coordinate list. The same user coordinates configure `ttf-parser`
   before every advance, bounding-box, ascender, descender, wrapping, collision, and fitting query,
   and instantiate the subsetter's outlines and metrics. Empty-coordinate fonts retain the legacy
   subset path and bytes. Output-font identities and coordinates do not enter translation or
   terminology cache keys.

## Production manifest

The primary slots remain the pinned Noto Sans SC 2.004 variable subset already used to derive the
corpus fixtures. Both logical weights map to that one SHA-256-pinned variable font and cache entry.
Regular retains the established default-location subset while Bold resolves its named `wght=700`
instance. The resulting static PDF subsets therefore carry distinct, metric-consistent outlines
without duplicating the downloaded asset or changing ordinary body-text bytes.

The 2026-08-27 L5 run exposed seven translated paragraphs containing `U+2217 ASTERISK OPERATOR`,
`U+0141 LATIN CAPITAL LETTER L WITH STROKE`, or `U+03F5 GREEK LUNATE EPSILON SYMBOL`. Noto Sans
2.015 was evaluated as the suggested fallback, but it covers only the latter two. DejaVu Sans 2.35
covers all three and is therefore the production fallback family. Its manifest entries are pinned
to Matplotlib tag `v3.11.1`:

- Regular URL:
  `https://raw.githubusercontent.com/matplotlib/matplotlib/v3.11.1/lib/matplotlib/mpl-data/fonts/ttf/DejaVuSans.ttf`
  with SHA-256 `3fdf69cabf06049ea70a00b5919340e2ce1e6d02b0cc3c4b44fb6801bd1e0d22`.
- Bold URL:
  `https://raw.githubusercontent.com/matplotlib/matplotlib/v3.11.1/lib/matplotlib/mpl-data/fonts/ttf/DejaVuSans-Bold.ttf`
  with SHA-256 `b184b89e3c1075f22f6b71575b6fc20d4972b3cfd3b23322ca6fd596dcaef167`.

Replacing these two manifest entries with independently pinned static Regular and Bold assets is a
required follow-up before claiming final typography parity. It does not change the resolution or
injection contract above. M4 still owns `assets pull`, model assets, progress UX, and a unified
public manifest surface.

## Consequences

- The production binary no longer contains the tiny corpus fixture font or any other output font.
- Offline users can provide all four slots explicitly; normal startup downloads and caches verified
  assets on first use.
- A real translation can lose only paragraphs whose primary and fallback fonts both lack required
  glyphs, and the diagnostic identifies the missing sample and both exact font identities.
- CI remains offline: download behavior is exercised only against a loopback HTTP server, then the
  same assets are resolved from cache after that server stops.
- Variable-font style no longer splits planning from publication: `/W`, extractor positions,
  wrapping, the 8 pt floor, CropBox checks, and collision checks all observe the instantiated slot.
