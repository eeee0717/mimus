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

## License

MIT — see [LICENSE](LICENSE).
