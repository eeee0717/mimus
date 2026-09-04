#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
work_dir="$(mktemp -d "${TMPDIR:-/tmp}/mimus-rust-licenses.XXXXXX")"
trap 'rm -rf "$work_dir"' EXIT

cd "$repo_root"
cargo tree --locked --offline -p mimus --edges normal --prefix none --format '{p}' \
  | sed -E 's/ \(\*\)$//; s/ \(proc-macro\)$//' \
  | sort -u > "$work_dir/tree.txt"
cargo metadata --locked --offline --format-version 1 > "$work_dir/metadata.json"

printf 'Rust normal-dependency license inventory\n'
printf 'Generated from Cargo.lock for the mimus package.\n\n'
printf 'name\tversion\tlicense\tsource\n'
jq -r --rawfile tree "$work_dir/tree.txt" '
  ($tree | split("\n")) as $included
  | .packages[]
  | select(.source != null)
  | (.name + " v" + .version) as $identity
  | select($included | index($identity))
  | [
      .name,
      .version,
      (.license // "UNKNOWN"),
      (.repository // .homepage // .source)
    ]
  | @tsv
' "$work_dir/metadata.json" | sort -u
