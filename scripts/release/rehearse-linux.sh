#!/usr/bin/env bash
set -euo pipefail

usage() {
  echo "usage: rehearse-linux.sh --archive TAR_GZ --sha256 HEX --input PDF --env-file FILE --output NEW_DIR" >&2
  exit 2
}

archive=''
expected_sha256=''
input_pdf=''
env_file=''
output_dir=''
while [[ $# -gt 0 ]]; do
  case "$1" in
    --archive) archive="$2"; shift 2 ;;
    --sha256) expected_sha256="$2"; shift 2 ;;
    --input) input_pdf="$2"; shift 2 ;;
    --env-file) env_file="$2"; shift 2 ;;
    --output) output_dir="$2"; shift 2 ;;
    *) usage ;;
  esac
done
[[ -n "$archive" && -n "$expected_sha256" && -n "$input_pdf" && -n "$env_file" && -n "$output_dir" ]] || usage

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
archive="$(cd "$(dirname "$archive")" && pwd)/$(basename "$archive")"
input_pdf="$(cd "$(dirname "$input_pdf")" && pwd)/$(basename "$input_pdf")"
env_file="$(cd "$(dirname "$env_file")" && pwd)/$(basename "$env_file")"
output_parent="$(cd "$(dirname "$output_dir")" && pwd)"
output_dir="$output_parent/$(basename "$output_dir")"

[[ -f "$archive" && -f "$input_pdf" && -f "$env_file" ]] || usage
[[ "$(basename "$input_pdf")" == '04-mobilenets.pdf' ]] || {
  echo "the M4 rehearsal input must be archived 04-mobilenets.pdf" >&2
  exit 2
}
[[ ! -e "$output_dir" ]] || { echo "output already exists: $output_dir" >&2; exit 2; }
command -v docker > /dev/null || { echo "Docker is required" >&2; exit 2; }
command -v python3 > /dev/null || { echo "Python 3 is required by the host-only proxy" >&2; exit 2; }
command -v curl > /dev/null || { echo "curl is required" >&2; exit 2; }
command -v jq > /dev/null || { echo "jq is required" >&2; exit 2; }

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

mkdir "$output_dir"
mkdir -p "$output_dir/skill-source/skills"
cp -R "$repo_root/skills/mimus" "$output_dir/skill-source/skills/mimus"
proxy_pid=''
cleanup() {
  if [[ -n "$proxy_pid" ]]; then
    kill "$proxy_pid" 2>/dev/null || true
    wait "$proxy_pid" 2>/dev/null || true
  fi
}
trap cleanup EXIT

python3 "$repo_root/scripts/release/counting-proxy.py" \
  --env-file "$env_file" \
  --limit 300 \
  --log "$output_dir/proxy.ndjson" \
  --port-file "$output_dir/proxy.port" \
  --client-key-file "$output_dir/proxy.client-key" \
  --scan-root "$output_dir" \
  --bind 0.0.0.0 \
  > "$output_dir/proxy.stdout" \
  2> "$output_dir/proxy.stderr" &
proxy_pid=$!
for _attempt in {1..100}; do
  [[ -s "$output_dir/proxy.port" && -s "$output_dir/proxy.client-key" ]] && break
  kill -0 "$proxy_pid" 2>/dev/null || { echo "counting proxy exited during startup" >&2; exit 1; }
  sleep 0.1
done
[[ -s "$output_dir/proxy.port" && -s "$output_dir/proxy.client-key" ]] || {
  echo "counting proxy did not become ready" >&2
  exit 1
}
proxy_port="$(cat "$output_dir/proxy.port")"
proxy_client_key="$(cat "$output_dir/proxy.client-key")"

docker pull --platform linux/amd64 ubuntu:24.04 > "$output_dir/docker-pull.log"
docker run --rm \
  --platform linux/amd64 \
  --add-host host.docker.internal:host-gateway \
  --env "PROXY_PORT=$proxy_port" \
  --env "API_KEY=$proxy_client_key" \
  --env MODEL_ID=m35-proxy-model \
  --env MIMUS_CACHE_DIR=/evidence/cache \
  --env "HOST_UID=$(id -u)" \
  --env "HOST_GID=$(id -g)" \
  --mount "type=bind,src=$archive,dst=/inputs/mimus.tar.gz,readonly" \
  --mount "type=bind,src=$input_pdf,dst=/inputs/04-mobilenets.pdf,readonly" \
  --mount "type=bind,src=$output_dir,dst=/evidence" \
  ubuntu:24.04 bash -s <<'CONTAINER'
set -euo pipefail
export DEBIAN_FRONTEND=noninteractive
apt-get update > /evidence/container-setup.log
apt-get install --yes ca-certificates curl jq nodejs npm qpdf time >> /evidence/container-setup.log

mkdir /evidence/release
tar -xzf /inputs/mimus.tar.gz -C /evidence/release
release_root="$(find /evidence/release -mindepth 1 -maxdepth 1 -type d -name 'mimus-*' -print -quit)"
[[ -n "$release_root" && -x "$release_root/mimus" ]]
mimus="$release_root/mimus"
"$mimus" --version > /evidence/version.txt
"$mimus" --help > /evidence/help.txt

validate_ndjson() {
  local stream="$1"
  jq -se '
    length > 0 and
    all(.[]; .schema_version == 2) and
    ([.[] | select(.event == "result" or .event == "error")] | length == 1) and
    (last.event == "result")
  ' "$stream" > /dev/null
}

mkdir /evidence/cache
/usr/bin/time -f 'elapsed_seconds=%e\nmax_rss_kib=%M' -o /evidence/assets.time \
  "$mimus" --json assets pull > /evidence/assets.ndjson 2> /evidence/assets.stderr
validate_ndjson /evidence/assets.ndjson
test "$(tail -n 1 /evidence/assets.ndjson | jq '.assets | length')" -eq 4
du -sb /evidence/cache > /evidence/assets.bytes

export BASE_URL="http://host.docker.internal:$PROXY_PORT/v1"
/usr/bin/time -f 'elapsed_seconds=%e\nmax_rss_kib=%M' -o /evidence/real-translate.time \
  "$mimus" --json translate /inputs/04-mobilenets.pdf \
    --output /evidence/04-mobilenets.zh.pdf \
    --cache /evidence/04-mobilenets.cache.redb \
    --request-timeout 600 \
    > /evidence/real-translate.ndjson 2> /evidence/real-translate.stderr
validate_ndjson /evidence/real-translate.ndjson
qpdf --check /evidence/04-mobilenets.zh.pdf > /evidence/real-translate.qpdf.txt 2>&1
test "$(qpdf --show-npages /evidence/04-mobilenets.zh.pdf)" -eq 9
curl --fail --silent --header "Authorization: Bearer $API_KEY" \
  "http://host.docker.internal:$PROXY_PORT/count" > /evidence/proxy-after-real.json
test "$(jq -r .forwarded /evidence/proxy-after-real.json)" -gt 0
test "$(jq -r .rejected /evidence/proxy-after-real.json)" -eq 0

mkdir /evidence/agent
cd /evidence/agent
npx --yes skills add /evidence/skill-source --skill mimus --agent codex --copy -y \
  > /evidence/skills-add.log
test -f .agents/skills/mimus/SKILL.md
"$mimus" --json inspect /inputs/04-mobilenets.pdf \
  > /evidence/agent-inspect.ndjson 2> /evidence/agent-inspect.stderr
validate_ndjson /evidence/agent-inspect.ndjson
"$mimus" --json translate /inputs/04-mobilenets.pdf \
  --output /evidence/04-mobilenets.cache-replay.zh.pdf \
  --cache /evidence/04-mobilenets.cache.redb \
  --request-timeout 600 \
  > /evidence/agent-translate.ndjson 2> /evidence/agent-translate.stderr
validate_ndjson /evidence/agent-translate.ndjson
qpdf --check /evidence/04-mobilenets.cache-replay.zh.pdf \
  > /evidence/agent-translate.qpdf.txt 2>&1
curl --fail --silent --header "Authorization: Bearer $API_KEY" \
  "http://host.docker.internal:$PROXY_PORT/count" > /evidence/proxy-after-agent.json
test "$(jq -r .forwarded /evidence/proxy-after-real.json)" \
  -eq "$(jq -r .forwarded /evidence/proxy-after-agent.json)"
test "$(jq -r .rejected /evidence/proxy-after-agent.json)" -eq 0
chmod 0444 /evidence/04-mobilenets.cache.redb
chown -R "$HOST_UID:$HOST_GID" /evidence
CONTAINER

curl --fail --silent --header "Authorization: Bearer $proxy_client_key" \
  "http://127.0.0.1:$proxy_port/leak-check" > "$output_dir/leak-check.json"
jq -e '.clean and (.matches | length == 0) and (.errors | length == 0)' \
  "$output_dir/leak-check.json" > /dev/null
curl --fail --silent --header "Authorization: Bearer $proxy_client_key" \
  "http://127.0.0.1:$proxy_port/count" > "$output_dir/proxy-final.json"

input_sha256="$(sha256_file "$input_pdf")"
jq -n \
  --arg version "$(cat "$output_dir/version.txt")" \
  --arg archive_sha256 "$actual_sha256" \
  --arg input_sha256 "$input_sha256" \
  --arg cache_path "$output_dir/04-mobilenets.cache.redb" \
  --rawfile assets_size "$output_dir/assets.bytes" \
  --rawfile assets_time "$output_dir/assets.time" \
  --rawfile translate_time "$output_dir/real-translate.time" \
  --slurpfile assets "$output_dir/assets.ndjson" \
  --slurpfile real "$output_dir/real-translate.ndjson" \
  --slurpfile inspect "$output_dir/agent-inspect.ndjson" \
  --slurpfile replay "$output_dir/agent-translate.ndjson" \
  --slurpfile after_real "$output_dir/proxy-after-real.json" \
  --slurpfile proxy "$output_dir/proxy-final.json" \
  --slurpfile leak "$output_dir/leak-check.json" \
  '{
    version: $version,
    archive_sha256: $archive_sha256,
    input: {name: "04-mobilenets.pdf", sha256: $input_sha256, pages: 9},
    assets: {
      count: ($assets[-1].assets | length),
      cache_bytes: ($assets_size | split(" ")[0] | tonumber),
      resource_usage: ($assets_time | rtrimstr("\n"))
    },
    real_translate_resource_usage: ($translate_time | rtrimstr("\n")),
    terminal_events: {
      assets_pull: $assets[-1].event,
      real_translate: $real[-1].event,
      agent_inspect: $inspect[-1].event,
      agent_cache_replay: $replay[-1].event
    },
    forwarded_after_real_translate: $after_real[0].forwarded,
    cache_replay: {
      hits: ([$replay[] | select(.event == "translation_cache" and .status == "hit")] | length),
      misses: ([$replay[] | select(.event == "translation_cache" and .status == "miss")] | length),
      added_provider_calls: ($proxy[0].forwarded - $after_real[0].forwarded)
    },
    proxy: $proxy[0],
    key_scan: $leak[0],
    immutable_cache: $cache_path
  }' > "$output_dir/summary.json"

echo "M4 Linux release rehearsal passed: $output_dir"
