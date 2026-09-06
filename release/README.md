# mimus release archive

The supported release target is macOS arm64 (Apple Silicon). macOS x64, Linux x64, and Windows x64
archives are preview/best-effort builds without a compatibility guarantee; they have passed hosted
CI build, dependency audit, and real-model smoke tests but not maintainer-controlled native
clean-machine and manual visual acceptance.

This archive contains the `mimus` command and the matching PDFium dynamic library. The macOS x64
archive also contains the matching ONNX Runtime dynamic library. The Windows archive includes the
four pinned Microsoft Visual C++ Runtime DLLs imported by `mimus.exe`: `msvcp140.dll`,
`msvcp140_1.dll`, `vcruntime140.dll`, and `vcruntime140_1.dll`. Keep the executable and all adjacent
libraries in the same directory. No Python or Node.js runtime is required.

Verify the download against the release `SHA256SUMS`, then run:

```sh
./mimus --version
./mimus assets pull
./mimus --json inspect paper.pdf
./mimus translate paper.pdf
```

On Windows, use `mimus.exe` instead of `./mimus`. The Windows build requires Windows 10 version
1903 or later, or Windows 11, because its ONNX Runtime backend imports DirectML, D3D12, and DXGI.

The PP-DocLayoutV3 model and the Noto Serif SC / STIX Two fonts are SHA-256-pinned runtime assets;
they are intentionally not included here. `mimus assets pull` downloads them before an offline
session. Set `MIMUS_ASSET_MIRROR` for a mirror or use the documented local model and font settings.

See the project README for configuration, API key, glossary, bilingual, and debugging workflows.
`DEPENDENCIES.txt` records the platform dependency audit performed while this archive was built.
`THIRD_PARTY_NOTICES` and `licenses/` contain the applicable notices.
