# mimus release archive

This archive contains the `mimus` command and the matching PDFium dynamic library. The macOS x64
archive also contains the matching ONNX Runtime dynamic library. Keep the executable and all
adjacent libraries in the same directory. No Python or Node.js runtime is required.

Verify the download against the release `SHA256SUMS`, then run:

```sh
./mimus --version
./mimus assets pull
./mimus --json inspect paper.pdf
./mimus translate paper.pdf
```

On Windows, use `mimus.exe` instead of `./mimus`.

The PP-DocLayoutV3 model and the Noto Serif SC / STIX Two fonts are SHA-256-pinned runtime assets;
they are intentionally not included here. `mimus assets pull` downloads them before an offline
session. Set `MIMUS_ASSET_MIRROR` for a mirror or use the documented local model and font settings.

See the project README for configuration, API key, glossary, bilingual, and debugging workflows.
`DEPENDENCIES.txt` records the platform dependency audit performed while this archive was built.
`THIRD_PARTY_NOTICES` and `licenses/` contain the applicable notices.
