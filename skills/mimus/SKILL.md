---
name: mimus
description: Use when translating or inspecting PDFs with the mimus CLI, preparing its runtime assets, or handling its versioned machine-protocol results.
compatibility: Requires mimus >=0.1.0-alpha.1 and <0.2.0 on PATH.
---

# Mimus

Drive the installed `mimus` CLI through schema-v2 NDJSON. The CLI is the behavior source; consult
`mimus <command> --help` for current options instead of reproducing its parameter manual here.

## Preflight

1. Run `command -v mimus`. When it is absent, stop and direct the user to the matching archive at
   <https://github.com/eeee0717/mimus/releases>. Leave binary and asset installation to the user.
2. Run `mimus --version` and require `>=0.1.0-alpha.1 and <0.2.0`. When it is outside that range, stop and
   direct the user to a compatible Release.
3. Choose the workflow below. Read command-specific help only when options beyond the shown core
   invocation are needed.

## Machine Protocol

For each invocation, parse stdout as NDJSON objects; do not infer state from terminal prose. Require
`schema_version: 2` on every event, exactly one `result` or `error` terminal event, and that terminal
event as the final line. Use the process exit code as the primary outcome and the terminal event for
typed detail. Treat malformed JSON, an absent terminal event, or output after the terminal event as
a protocol failure.

## Prepare Assets

Run:

```sh
mimus --json assets pull
```

Success is exit code 0 followed by one final `result`; its `assets` entries are the authoritative
ready set. Report typed `Asset/3` failures without installing or substituting assets yourself.

## Inspect

Run:

```sh
mimus --json inspect INPUT.pdf
```

On success, consume the IL from the final `result`. Inspection does not need translation credentials.

## Translate

Before a remote translation, check only whether a supported environment variable is set or the
config file contains an `api_key` assignment. This check is silent and value-blind:

```sh
mimus_key_configured=0
if [[ -n ${MIMUS_OPENAI_API_KEY+x} || -n ${OPENAI_API_KEY+x} || -n ${API_KEY+x} ]]; then
  mimus_key_configured=1
elif [[ -f "$HOME/.config/mimus/config.toml" ]] &&
  awk '/^[[:space:]]*api_key[[:space:]]*=/ { found=1 } END { exit !found }' \
    "$HOME/.config/mimus/config.toml"; then
  mimus_key_configured=1
fi
```

Never print, interpolate, copy, persist, or place a credential in a command, prompt, log, or result.
When `mimus_key_configured` is 0, stop and ask the user to configure a credential. Do not open or
display the config file. The CLI performs the definitive non-empty credential validation.

Run:

```sh
mimus --json translate INPUT.pdf
```

Success is exit code 0 followed by one final `result`; take the published output path from that
event. For a deliberate offline identity run, select the `none` backend through the option shown by
`mimus translate --help`; it does not require a credential.
