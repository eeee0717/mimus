# Public release checklist

M4 prepares release candidates but does not create a tag or GitHub Release. The maintainer owns the
following irreversible/public steps.

## Before tagging

- Merge the M4 pull requests in dependency order: assets, archives, Agent Skill, rehearsal/docs.
- Resolve the Linux container rehearsal follow-up (#187) and attach its `summary.json` to #42.
- Choose the public version. The tag must exactly match the Cargo version as `v<version>`; if Cargo
  remains `0.0.0`, the matching tag is `v0.0.0`. A `v0.1.0` release requires updating Cargo and the
  Agent Skill compatibility range, then rebuilding all artifacts.
- Confirm CI and the release archive matrix are green at the exact commit to tag.
- Preserve the immutable `04-mobilenets.cache.redb` recorded by the Linux rehearsal before removing
  its workspace.

## Publish

1. Create and push the chosen tag. Do not create a GitHub Release by hand.
2. Watch `Release archives`: all four platform build jobs and combined checksum verification must
   pass. The tag-only publish job then creates the Release and uploads four archives plus
   `SHA256SUMS`.
3. Confirm each archive name/version, checksum, dependency audit, license tree, and inspect NDJSON
   artifact matches the tagged commit.

## Clean-machine acceptance

Use machines without an existing mimus cache. Keep the generated evidence directories.

### macOS arm64 and x64

- Download the matching tarball and `SHA256SUMS` from the published Release.
- Install `jq`, qpdf, Node.js, and `npx` only as acceptance tools.
- Run `scripts/release/verify-install.sh` with the archive, its checksum, a native test PDF, a new
  evidence directory, and `eeee0717/mimus` as the skill source.
- Confirm `summary.json` reports four assets, five `result` terminal events, qpdf success, and skill
  installation success. Open `roundtrip.pdf` and inspect every page.
- On Intel macOS, confirm `libonnxruntime.1.23.2.dylib` remains beside `mimus` and the dependency
  audit resolves it through `@executable_path`; Apple Silicon links ONNX Runtime statically.
- Repeat on both Apple Silicon and Intel hardware; do not substitute Rosetta for either native run.

### Windows x64

- Download the zip and `SHA256SUMS` from the published Release.
- Install qpdf, Node.js, and `npx` only as acceptance tools.
- Run `scripts/release/verify-install.ps1` with the archive, checksum, native test PDF, a new evidence
  directory, and `eeee0717/mimus` as the skill source.
- Confirm `summary.json` reports four assets, five `result` terminal events, qpdf success, and skill
  installation success. Open `roundtrip.pdf` and inspect every page.
- Confirm `mimus.exe`, `pdfium.dll`, `msvcp140.dll`, `msvcp140_1.dll`, `vcruntime140.dll`, and
  `vcruntime140_1.dll` remain in the same directory and the CLI runs without a separately installed
  VC++ Redistributable. No Python or Node.js process is involved after the Agent Skill installation
  check.

Any platform failure blocks announcing the Release. Preserve the archive, input SHA, NDJSON,
`DEPENDENCIES.txt`, qpdf output, and evidence directory when filing the defect.
