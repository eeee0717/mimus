#!/usr/bin/env bash
set -euo pipefail

version="${1:-}"
[[ -n "$version" ]] || {
  echo "usage: check-tag-version.sh VERSION" >&2
  exit 2
}

if [[ "${GITHUB_REF_TYPE:-}" != tag ]]; then
  exit 0
fi

expected_tag="v${version}"
if [[ "${GITHUB_REF_NAME:-}" != "$expected_tag" ]]; then
  echo "release tag/version mismatch: expected $expected_tag, got ${GITHUB_REF_NAME:-<unset>}" >&2
  exit 1
fi
