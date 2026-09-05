#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 5 ]]; then
  echo "usage: verify-install.sh ARCHIVE EXPECTED_SHA256 INPUT_PDF NEW_EVIDENCE_DIR SKILL_SOURCE" >&2
  exit 2
fi

archive="$1"
expected_sha256="$2"
input_pdf="$3"
evidence="$4"
skill_source="$5"
[[ -f "$archive" && -f "$input_pdf" ]] || { echo "archive and input PDF must exist" >&2; exit 2; }
[[ ! -e "$evidence" ]] || { echo "evidence directory already exists: $evidence" >&2; exit 2; }
for command_name in jq npx qpdf; do
  command -v "$command_name" > /dev/null || { echo "$command_name is required by the acceptance harness" >&2; exit 2; }
done

sha256_file() {
  if command -v sha256sum > /dev/null 2>&1; then
    sha256sum "$1" | awk '{print $1}'
  else
    shasum -a 256 "$1" | awk '{print $1}'
  fi
}

actual_sha256="$(sha256_file "$archive")"
[[ "$actual_sha256" == "$expected_sha256" ]] || {
  echo "archive SHA-256 mismatch: expected $expected_sha256, got $actual_sha256" >&2
  exit 1
}

mkdir "$evidence"
tar -xzf "$archive" -C "$evidence"
release_root="$(find "$evidence" -mindepth 1 -maxdepth 1 -type d -name 'mimus-*' -print -quit)"
[[ -n "$release_root" && -x "$release_root/mimus" ]] || { echo "archive has no mimus executable" >&2; exit 1; }
mimus="$release_root/mimus"
export MIMUS_CACHE_DIR="$evidence/cache"

validate_ndjson() {
  local stream="$1"
  jq -se '
    length > 0 and
    all(.[]; .schema_version == 2) and
    ([.[] | select(.event == "result" or .event == "error")] | length == 1) and
    (last.event == "result")
  ' "$stream" > /dev/null
}

"$mimus" --help > "$evidence/help.txt"
"$mimus" --version > "$evidence/version.txt"
assets_started="$(date +%s)"
"$mimus" --json assets pull > "$evidence/assets.ndjson" 2> "$evidence/assets.stderr"
assets_elapsed="$(( $(date +%s) - assets_started ))"
validate_ndjson "$evidence/assets.ndjson"
test "$(tail -n 1 "$evidence/assets.ndjson" | jq '.assets | length')" -eq 4

"$mimus" --json inspect "$input_pdf" \
  --debug "$evidence/inspect-debug" > "$evidence/inspect.ndjson" 2> "$evidence/inspect.stderr"
validate_ndjson "$evidence/inspect.ndjson"
"$mimus" --json translate "$input_pdf" \
  --backend none \
  --output "$evidence/roundtrip.pdf" \
  --bilingual \
  --strip-link-borders \
  > "$evidence/translate.ndjson" 2> "$evidence/translate.stderr"
validate_ndjson "$evidence/translate.ndjson"
qpdf --check "$evidence/roundtrip.pdf" > "$evidence/qpdf.txt" 2>&1

mkdir "$evidence/agent"
(
  cd "$evidence/agent"
  npx --yes skills add "$skill_source" --skill mimus --agent codex --copy -y
) > "$evidence/skills-add.log"
test -f "$evidence/agent/.agents/skills/mimus/SKILL.md"
"$mimus" --json inspect "$input_pdf" > "$evidence/agent-inspect.ndjson" 2> "$evidence/agent-inspect.stderr"
validate_ndjson "$evidence/agent-inspect.ndjson"
"$mimus" --json translate "$input_pdf" \
  --backend none \
  --output "$evidence/agent-roundtrip.pdf" \
  > "$evidence/agent-translate.ndjson" 2> "$evidence/agent-translate.stderr"
validate_ndjson "$evidence/agent-translate.ndjson"
qpdf --check "$evidence/agent-roundtrip.pdf" > "$evidence/agent-qpdf.txt" 2>&1

cache_bytes="$(du -sk "$evidence/cache" | awk '{print $1 * 1024}')"
input_sha256="$(sha256_file "$input_pdf")"
jq -n \
  --arg version "$(cat "$evidence/version.txt")" \
  --arg archive_sha256 "$actual_sha256" \
  --arg input_sha256 "$input_sha256" \
  --argjson assets_elapsed_seconds "$assets_elapsed" \
  --argjson asset_cache_bytes "$cache_bytes" \
  '{
    version: $version,
    archive_sha256: $archive_sha256,
    input_sha256: $input_sha256,
    asset_count: 4,
    asset_cache_bytes: $asset_cache_bytes,
    assets_elapsed_seconds: $assets_elapsed_seconds,
    terminal_events: {
      assets_pull: "result",
      inspect: "result",
      translate: "result",
      agent_inspect: "result",
      agent_translate: "result"
    },
    qpdf: "passed",
    skill_install: "passed"
  }' > "$evidence/summary.json"

echo "release install verification passed: $evidence"
