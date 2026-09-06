# mimus

Layout-preserving PDF translation as a Rust CLI. Agent users can install a thin companion skill
with `npx skills add eeee0717/mimus --skill mimus`; it invokes the same CLI through versioned NDJSON.

See [release installation and usage](docs/13-release-and-usage.md) for archive setup, provider
configuration, offline assets, translation workflows, diagnostics, and known limits.

## Platform support

The supported release target is **macOS arm64 (Apple Silicon)**. macOS x64, Linux x64, and Windows
x64 archives are published as preview/best-effort builds: their hosted CI build, dependency audit,
and real-model smoke tests pass, but they have not completed maintainer-controlled native
clean-machine and manual visual acceptance. They do not carry a compatibility guarantee yet.

The current alpha can be installed on Apple Silicon directly from its GitHub Release through mise;
the prerelease opt-in is required:

```sh
mise use -g 'github:eeee0717/mimus[prerelease=true]@0.1.0-alpha.1'
mimus --version
```

Mimus is not yet listed in the official mise registry. The direct `github:` backend above is the
supported mise route for this alpha. Official registry submission is tracked in
[#196](https://github.com/eeee0717/mimus/issues/196) for stable `0.1.0`. See the release guide for
manual archive verification.

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
mimus --json assets list
mimus assets pull
```

Missing assets are downloaded on demand. `--asset-mirror`, `MIMUS_ASSET_MIRROR`, or
`asset_mirror` in the config file selects an HTTP(S) mirror; `MIMUS_CACHE_DIR` or `cache_dir`
selects the cache root. Explicit model and font paths bypass downloads. Every managed download is
size-bounded, SHA-256 checked, and atomically published only after validation.

## Quick start

```sh
mimus --json assets pull
mimus --json inspect paper.pdf
mimus --json translate paper.pdf --output paper.zh.pdf
```

The default translation backend needs an API key in the environment or user config. The key has no
CLI flag and is never included in resolved-configuration events. See the usage guide for the exact
precedence and glossary, bilingual, link-border, and debug workflows.

## License

MIT — see [LICENSE](LICENSE).
