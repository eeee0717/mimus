#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
check="$script_dir/check-tag-version.sh"

if GITHUB_REF_TYPE=tag GITHUB_REF_NAME=v9.9.9 "$check" 0.0.0 >/dev/null 2>&1; then
  echo "mismatched release tag unexpectedly passed" >&2
  exit 1
fi

GITHUB_REF_TYPE=tag GITHUB_REF_NAME=v0.0.0 "$check" 0.0.0
GITHUB_REF_TYPE=tag GITHUB_REF_NAME=v0.1.0-alpha.1 "$check" 0.1.0-alpha.1
if GITHUB_REF_TYPE=tag GITHUB_REF_NAME=v0.1.0 "$check" 0.1.0-alpha.1 >/dev/null 2>&1; then
  echo "stable tag unexpectedly matched a prerelease version" >&2
  exit 1
fi
GITHUB_REF_TYPE=branch GITHUB_REF_NAME=m4-release "$check" 0.0.0
printf 'tag/version gate accepts exact versions, rejects mismatches, and preserves non-tag runs\n'
