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

Translated text defaults to the SHA-pinned Noto Serif SC variable font (宋体), with Regular and
Bold instantiated at `wght=400` and `wght=700`. To use a sans-serif/黑体 face instead, provide its
Regular and Bold slots explicitly; one variable font may serve both:

```sh
mimus translate --font /path/to/NotoSansSC-VF.ttf \
  --font-bold /path/to/NotoSansSC-VF.ttf input.pdf
```

The existing flag, environment, config, cache, and asset-mirror precedence is unchanged. DejaVu
Sans 2.35 remains the fallback for characters absent from the primary CJK font.

## License

MIT — see [LICENSE](LICENSE).
