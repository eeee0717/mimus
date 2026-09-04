#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
skill="$repo_root/skills/mimus/SKILL.md"
metadata="$repo_root/skills/mimus/agents/openai.yaml"

fail() {
  echo "mimus Agent Skill validation failed: $*" >&2
  exit 1
}

[[ -f "$skill" ]] || fail "missing SKILL.md"
[[ -f "$metadata" ]] || fail "missing agents/openai.yaml"
[[ "$(sed -n '1p' "$skill")" == '---' ]] || fail "frontmatter must start on line 1"
[[ "$(awk 'NR > 1 && $0 == "---" { print NR; exit }' "$skill")" == 5 ]] || \
  fail "frontmatter must contain exactly the required four lines"

grep -Fx 'name: mimus' "$skill" > /dev/null || fail "frontmatter name must be mimus"
grep -Eq '^description: .+' "$skill" || fail "frontmatter description is required"
grep -Fx 'compatibility: Requires mimus >=0.0.0 and <0.1.0 on PATH.' "$skill" > /dev/null || \
  fail "CLI semver compatibility range is missing"

for invocation in \
  'mimus --json assets pull' \
  'mimus --json inspect INPUT.pdf' \
  'mimus --json translate INPUT.pdf'; do
  grep -Fx "$invocation" "$skill" > /dev/null || fail "missing machine invocation: $invocation"
done

if grep -Eq '^mimus (assets|inspect|translate)([[:space:]]|$)' "$skill"; then
  fail "workflow invocations must put --json before the subcommand"
fi
if grep -Eiq -- '--api[-_]key|printenv|env[[:space:]]+\|' "$skill"; then
  fail "skill must not accept or print credential values"
fi

for required in \
  'command -v mimus' \
  'schema_version: 2' \
  'exactly one `result` or `error` terminal event' \
  'Never print, interpolate, copy, persist, or place a credential' \
  'Leave binary and asset installation to the user.'; do
  grep -F "$required" "$skill" > /dev/null || fail "missing contract: $required"
done

grep -Fx '  display_name: "Mimus"' "$metadata" > /dev/null || fail "missing display name"
grep -Eq '^  short_description: ".+"$' "$metadata" || fail "missing short description"

echo "mimus Agent Skill structure is valid"
