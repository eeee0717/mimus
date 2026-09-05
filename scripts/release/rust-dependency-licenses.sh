#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
[[ -n "${RUST_TARGET:-}" ]] || {
  echo "RUST_TARGET is required" >&2
  exit 2
}

cd "$repo_root"
printf 'Rust normal-dependency license inventory\n'
printf 'Generated from Cargo.lock for the mimus package and target %s.\n\n' "$RUST_TARGET"
printf 'name\tversion\tlicense\tsource\n'
cargo tree --locked --offline -p mimus --target "$RUST_TARGET" \
  --edges normal --prefix none --format $'{p}\t{l}\t{r}' \
  | sed -E 's/ \(\*\)$//; s/ \(proc-macro\)\t/\t/' \
  | awk -F '\t' '
      {
        identity = $1
        if (!match(identity, / v[^[:space:]]+$/)) {
          next
        }
        name = substr(identity, 1, RSTART - 1)
        version = substr(identity, RSTART + 2)
        license = ($2 == "" ? "UNKNOWN" : $2)
        source = ($3 == "" ? "registry: crates.io" : $3)
        print name "\t" version "\t" license "\t" source
      }
    ' \
  | sort -u
