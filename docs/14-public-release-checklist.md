# Public release checklist

The release workflow builds four archives, but only macOS arm64 (Apple Silicon) is currently a
supported target. macOS x64, Linux x64, and Windows x64 are preview/best-effort artifacts until each
completes the maintainer-controlled acceptance below.

## Before tagging

- Merge the release-preparation pull request and confirm the target commit is on `master`.
- Cargo is `0.1.0-alpha.1`, so the matching tag is `v0.1.0-alpha.1`; tags containing `-` are
  automatically marked as pre-releases. When promoting to a stable version, update the workspace
  version, lockfile entries, both Agent Skill compatibility declarations, and their validation
  expectation together, then rebuild all artifacts.
- Confirm CI and the release archive matrix are green at the exact commit to tag.

## Publish

1. Create and push the chosen tag. Do not create a GitHub Release by hand.
2. Watch `Release archives`: all four platform build jobs and combined checksum verification must
   pass. The tag-only publish job then creates the Release and uploads four archives plus
   `SHA256SUMS`.
3. Confirm each archive name/version, checksum, dependency audit, license tree, and inspect NDJSON
   artifact matches the tagged commit.

## Supported-platform acceptance

Use machines without an existing mimus cache. Keep the generated evidence directories.

### macOS arm64

- Download the matching tarball and `SHA256SUMS` from the published Release.
- Install `jq`, qpdf, Node.js, and `npx` only as acceptance tools.
- Run `scripts/release/verify-install.sh` with the archive, its checksum, a native test PDF, a new
  evidence directory, and `eeee0717/mimus` as the skill source.
- Confirm `summary.json` reports four assets, five `result` terminal events, qpdf success, and skill
  installation success. Open `roundtrip.pdf` and inspect every page.
- Confirm the process is native arm64 rather than running under Rosetta. Apple Silicon links ONNX
  Runtime statically; `libpdfium.dylib` must remain beside `mimus`.

Failure here blocks the macOS arm64 support claim. The four-platform automated release matrix must
also remain green before any archive is published.

## Preview promotion acceptance

These checks are required to promote a target from preview, but their absence does not block the
macOS arm64 release. Do not substitute emulation or a hosted CI runner for maintainer-controlled
native hardware.

### macOS x64

- Follow the macOS steps above with the x64 tarball on Intel hardware.
- Confirm `libonnxruntime.1.23.2.dylib` remains beside `mimus` and the dependency audit resolves it
  through `@executable_path`.
- Open `roundtrip.pdf`, inspect every page, and retain the complete evidence directory.

### Linux x64

- Complete #187 on a maintainer-controlled amd64 Docker host with the published Linux archive.
- Preserve `summary.json`, the real output, call ledger, key scan, and read-only cache; attach the
  summary to #42.
- Preserve the immutable `04-mobilenets.cache.redb` before removing the rehearsal workspace.
- Inspect every page of the real translated output before proposing promotion from preview.

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

A preview-platform failure must be reported and blocks promotion of that target, not the macOS
arm64 support claim. Preserve the archive, input SHA, NDJSON, `DEPENDENCIES.txt`, qpdf output, and
evidence directory when filing the defect.
