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

1. Typeset receives Regular and Bold output font bytes through `PassContext`. Production code does
   not use `include_bytes!` for output fonts and does not read or download fonts inside Typeset.
2. Each font slot resolves at CLI startup in this order: `--font` / `--font-bold`,
   `MIMUS_FONT_REGULAR` / `MIMUS_FONT_BOLD`, `font_regular` / `font_bold` in the user config,
   a SHA-256-validated cache entry, then a manifest download. `MIMUS_CACHE_DIR` or `cache_dir`
   selects the cache root. Otherwise the platform user cache directory is used.
3. `--asset-mirror`, `MIMUS_ASSET_MIRROR`, or `asset_mirror` replaces the manifest base URL. Mirror
   values must be credential-free HTTP(S) base URLs without a query or fragment. Downloads are
   bounded to 64 MiB, checked against the pinned SHA-256, and published atomically.
4. A missing, unreadable, malformed, oversized, or hash-mismatched font fails before PDFium and the
   pipeline with `Asset/output_font_unavailable` (exit 3). The hint names the `--font` and
   `--font-bold` escape hatch. `configuration_resolved` exposes each selected source and SHA-256 as
   additive CLI v2 fields.
5. Glyph coverage is checked against the same bytes later passed to the subsetter. A coverage miss
   preserves only the affected paragraph as `unsupported_font` and emits
   `unsupported_output_glyph` with page, paragraph reading order, a unique missing-character
   sample, font source, and SHA-256. Structural paragraph mismatches use `typeset_protocol`; an
   absent unambiguous content span uses `unlocatable`.
6. `corpus/fonts/MimusCJK*.ttf` remain input fixture fonts only. Output-font tests inject the
   deterministic GB2312 level-one-scale assets under `crates/mimus/tests/assets/fonts/` through the
   public path/config seam. Those files include their generation recipe, pinned hashes, and OFL,
   and are never linked into a production target.

## Transitional manifest

The offline recovery could verify only the pinned Noto Sans SC 2.004 variable subset already used
to derive the corpus fixtures. The initial production manifest therefore maps both logical slots
to that one SHA-256-pinned variable font and cache entry. This restores real CJK coverage but does
**not** yet provide distinct static Regular and Bold outlines; bold text uses the variable font's
default instance.

Replacing these two manifest entries with independently pinned static Regular and Bold assets is a
required follow-up before claiming final typography parity. It does not change the resolution or
injection contract above. M4 still owns `assets pull`, model assets, progress UX, and a unified
public manifest surface.

## Consequences

- The production binary no longer contains the tiny corpus fixture font or any other output font.
- Offline users can provide both weights explicitly; normal startup downloads and caches verified
  assets on first use.
- A real translation can lose only paragraphs whose selected font actually lacks required glyphs,
  and the diagnostic identifies both the missing sample and the exact font bytes.
- CI remains offline: download behavior is exercised only against a loopback HTTP server, then the
  same assets are resolved from cache after that server stops.
