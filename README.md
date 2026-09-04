# mimus

Layout-preserving PDF translation as a Rust CLI. Agent users will be able to install a thin companion skill with `npx skills add eeee0717/mimus`; it invokes the same CLI for agent-driven workflows.

Planning is active and implementation has just begun. See `CONTEXT.md` and `docs/`.

## Workspace

| Crate | Kind | Role |
|---|---|---|
| `crates/mimus-core` | lib | IL, pass chain, engine traits, translation layer |
| `crates/mimus` | bin (`mimus`) | CLI, progress, configuration |

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-targets --all-features
```

## Output fonts

Translated text uses two SHA-pinned font families. Han, CJK forms, and Chinese-context punctuation
prefer Noto Serif SC (宋体); ASCII, Latin, Greek, Cyrillic, numbers, and mathematical symbols prefer
STIX Two Text, with STIX Two Math as the same-family symbol fallback. Regular and Bold variable-font
slots are instantiated at `wght=400` and `wght=700`. Line metrics come only from the CJK family;
STIX glyphs use the same size and baseline without scaling.

To use custom fonts, provide both CJK and Latin weight slots; one variable font may serve both
weights in a family:

```sh
mimus translate --font /path/to/NotoSansSC-VF.ttf \
  --font-bold /path/to/NotoSansSC-VF.ttf \
  --font-latin /path/to/STIXTwoText.ttf \
  --font-latin-bold /path/to/STIXTwoText.ttf input.pdf
```

The precedence remains flag, environment, config, validated cache, then pinned manifest download.
The canonical Latin names are `--font-latin` / `--font-latin-bold`, `MIMUS_FONT_LATIN` /
`MIMUS_FONT_LATIN_BOLD`, and `font_latin` / `font_latin_bold`. The former `fallback` names remain
deprecated aliases for CLI v2 compatibility.

## Runtime assets

The public manifest contains the PP-DocLayoutV3 model and the three default font files above.
Inspect it without downloading anything, or prefetch every default asset into the existing cache:

```sh
mimus assets list --json
mimus assets pull
```

Missing assets are downloaded on demand. `--asset-mirror`, `MIMUS_ASSET_MIRROR`, or
`asset_mirror` in the config file selects an HTTP(S) mirror; `MIMUS_CACHE_DIR` or `cache_dir`
selects the cache root. Explicit model and font paths bypass downloads. Every managed download is
size-bounded, SHA-256 checked, and atomically published only after validation.

## License

MIT — see [LICENSE](LICENSE).
