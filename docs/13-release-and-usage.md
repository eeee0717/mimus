# Release installation and usage

Mimus is distributed as one platform archive containing the CLI, the matching PDFium dynamic
library, and license material. Models, fonts, and the Agent Skill remain separate runtime assets.

## Install an archive

Download the archive and `SHA256SUMS` from the same GitHub Release. Verify the archive before
extracting it. On macOS or Linux:

```sh
grep 'mimus-vVERSION-PLATFORM.tar.gz$' SHA256SUMS | shasum -a 256 -c -
tar -xzf mimus-vVERSION-PLATFORM.tar.gz
cd mimus-vVERSION-PLATFORM
./mimus --version
```

Linux may pipe the matching row to `sha256sum --check` instead. On Windows, compare `Get-FileHash`
with the matching `SHA256SUMS` row and use `Expand-Archive`. Keep `mimus`/`mimus.exe` and the adjacent
`libpdfium` file together. Python and Node.js are not runtime dependencies of the CLI.

For a recorded clean-machine smoke, use
[`verify-install.sh`](../scripts/release/verify-install.sh) on macOS/Linux or
[`verify-install.ps1`](../scripts/release/verify-install.ps1) on Windows. These acceptance harnesses
also require `jq`/PowerShell, qpdf, Node.js, and `npx`; the released CLI does not.

## First run

The first production run needs the layout model and three fonts. Inspect the public manifest or
prefetch all four files before going offline:

```sh
mimus --json assets list
mimus --json assets pull
mimus --json inspect paper.pdf
```

Every JSON-mode command writes schema-v2 NDJSON to stdout. A valid stream ends with exactly one
`result` or `error`; scripts should use the process exit code first and the terminal event for typed
detail. Human mode writes progress and diagnostics to stderr.

Translate after configuring a provider:

```sh
mimus --json translate paper.pdf --output paper.zh.pdf
```

For a credential-free pipeline and writeback check, add `--backend none`; this preserves source
text and does not represent translation quality.

## Configuration and credentials

Ordinary settings resolve in this order: command flag, environment variable, then
`~/.config/mimus/config.toml`, followed by the documented default. `endpoint`/`base_url` and
`model_id`/`model` are accepted config aliases. The API key deliberately has no command flag; it
resolves from `MIMUS_OPENAI_API_KEY`, `OPENAI_API_KEY`, or `API_KEY`, then `api_key` in the config
file. Empty keys are absent.

```toml
backend = "openai"
base_url = "https://api.openai.com"
model = "gpt-4.1-mini"
target_language = "zh-CN"
api_key = "configure-locally"
concurrency = 4
request_timeout_secs = 120
```

Keep credentials out of shell history, command arguments, bug reports, logs, and committed files.
JSON `configuration_resolved` events report the endpoint and model but never the key.

## Assets, mirrors, and offline paths

Managed assets live under the platform cache root; override it with `MIMUS_CACHE_DIR` or
`cache_dir`. `--asset-mirror`, `MIMUS_ASSET_MIRROR`, or `asset_mirror` selects an HTTP(S) mirror.
Downloads stream into a same-directory temporary file and become visible only after size,
SHA-256, and compatibility checks pass.

Explicit paths bypass the asset network completely:

```sh
mimus --json inspect paper.pdf --layout-model /srv/mimus/inference.onnx
mimus --json translate paper.pdf \
  --font /srv/mimus/NotoSerifSC-VF.ttf \
  --font-bold /srv/mimus/NotoSerifSC-VF.ttf \
  --font-latin '/srv/mimus/STIXTwoText[wght].ttf' \
  --font-latin-bold '/srv/mimus/STIXTwoText[wght].ttf' \
  --layout-model /srv/mimus/inference.onnx
```

The default CJK face is Noto Serif SC (宋体). Han and Chinese-context punctuation use it; Latin,
Greek, Cyrillic, ASCII, and numbers use STIX Two Text, with STIX Two Math for same-family symbol
fallback. To select a local black face, point `--font` and `--font-bold` at a compatible
`NotoSansSC-VF.ttf`. `--font-latin` and `--font-latin-bold` are the canonical Latin slots; the old
`fallback` names remain deprecated CLI-v2 aliases.

PDFium resolves from beside the executable, or from `MIMUS_PDFIUM_LIBRARY` for a custom install.

## Terminology and output modes

Automatic terminology runs by default. Export its canonical result, review it, then reuse it:

```sh
mimus --json translate paper.pdf --dump-glossary paper.glossary.toml
mimus --json translate paper.pdf --glossary paper.glossary.toml --no-auto-terms
```

User glossary entries override automatic entries. The final glossary fingerprint participates in
the translation cache key.

Use `--bilingual` to publish each original page followed by its translated page. Use
`--strip-link-borders` to hide visible Link annotation borders while preserving their target and
rectangle:

```sh
mimus --json translate paper.pdf --bilingual --strip-link-borders
```

## Inspection and debug evidence

`inspect` stops after ParagraphFind and never calls the translation backend. A debug directory must
not already exist; it retains canonical IL snapshots for each completed pass and a diagnostics
stream:

```sh
mimus --json inspect paper.pdf --debug inspect-debug
mimus --json translate paper.pdf --debug translate-debug
```

## Exit status

| Code | Category | Meaning |
| ---: | --- | --- |
| 0 | success | A result was published; inspect warnings and typed degradation. |
| 1 | Usage | Arguments or configuration are invalid. |
| 2 | Input | The PDF is invalid, encrypted, scanned, or unsupported. |
| 3 | Asset | PDFium, a model, or a font is unavailable or invalid. |
| 4 | Translation | Provider work failed, or `--strict` rejected degradation. |
| 5 | I/O | Input, cache, debug, or atomic output publication failed. |
| 6 | Internal | An invariant or output validation failed; report this as a bug. |

Normal mode preserves the affected paragraph/page when possible and may still return success;
`--strict` prevents publication when hard degradation occurred. It does not turn informational
identity or deliberate passthrough into an error.

## Agent Skill

Install the thin instruction package with `npx skills add eeee0717/mimus --skill mimus`. The skill
checks that a compatible `mimus` is already on `PATH` and drives `translate`, `inspect`, and
`assets pull` only through NDJSON. It neither installs the binary/assets nor reads credential
values.

## Known V1 limits

- Input must be a native PDF. Documents meeting the scan threshold are rejected; OCR is deferred.
- Encrypted PDFs are rejected, including files whose empty password would otherwise open them.
- Text owned only by a Form XObject, including common whole-page wrappers, is preserved and
  reported as `form_xobject_content`; mixed page/Form content can still translate page-owned units.
- Table text remains untranslated unless the experimental `--translate-table` flag is selected.
- Acceptance is tuned for English-to-Simplified-Chinese output. Other source languages may extract,
  but are outside the release quality claim.
- Final semantic correctness and visual appeal require human review. Typed diagnostics and the
  scorecard are triage evidence, not that verdict.

The measured coverage and remaining combinations are maintained in
[`docs/12-acceptance-gap-matrix.md`](12-acceptance-gap-matrix.md).
