# mimus

> _Mimus polyglottos_ — the northern mockingbird. Latin _mimus_, "mimic";
> Greek _polyglottos_, "many-tongued". Linnaeus named it in 1758, and the name
> happens to describe this program exactly: understand something in one
> language, reproduce it faithfully in another.

Translate a PDF while preserving its layout. A Rust CLI.

Status: **skeleton**. Nothing is wired up yet.

## Why this exists

[BabelDOC](https://github.com/funstory-ai/BabelDOC) solves this well in Python
(~38k lines of hand-written logic, 20 months, 22 contributors). `mimus` differs
on two axes:

- **Scanned documents.** BabelDOC raises `ScannedPDFError` and stops. `mimus`
  treats OCR as a first-class input path, not an afterthought.
- **Distribution.** A single binary instead of a Python environment carrying
  onnxruntime and PyMuPDF wheels.

## Architecture

```
crates/
  mimus-ir          document intermediate representation  (start here)
  mimus-pdf         PDFium for geometry, lopdf for incremental writes
  mimus-layout      RT-DETR / PP-DocLayout via ONNX Runtime
  mimus-ocr         PP-OCRv6 det + rec via ONNX Runtime
  mimus-translate   LLM translation, placeholder round-tripping, cache
  mimus-typeset     line breaking, scale search, glyph placement
  mimus-cli         the `mimus` binary
```

Every stage reads an IR document and writes one back. The IR is serialisable so
that stages can be snapshot-tested and pages can be handed between threads.

### Decisions worth not re-litigating

**Reading a PDF takes two passes, not one.** PDFium supplies glyph geometry and
font metrics — the part with the deepest edge cases (Type3 font matrices, CID
widths, missing-font fallbacks). It does *not* expose the raw operator stream,
so a second pass over the decompressed content stream supplies verbatim
graphics state, XObject nesting and exact draw order. Neither half is
sufficient alone. BabelDOC reached the same shape: its own 17k-line interpreter
sitting on top of MuPDF.

**Writing has two modes and they are not interchangeable.** Native PDFs need
*incremental* editing so images, vector art, annotations and bookmarks survive;
`krilla`-style from-scratch generation would discard them. Scanned PDFs are
already just rasters, so rebuilding from scratch is lossless there and much
simpler. Do not try to serve both with one writer.

**Character provenance belongs in the type.** See `CharSource` in `mimus-ir`.
Native characters can be re-emitted byte-identically and therefore passed
through untouched; OCR characters have no font identity, no colour and no
original operators, so they can only be redrawn — which is why the OCR path
covers the source with a filled rectangle rather than editing it. This
distinction reaches every downstream stage. Retrofitting it later is painful.

**Layout labels are data, never an enum.** DocLayout-YOLO ships 10 labels,
PP-DocLayout ships 20+, and an RPC backend can return anything. Rank by a
priority table keyed on strings.

**Layout detection needs a fallback.** Regions the model misses would otherwise
be dropped silently and never translated. BabelDOC clusters loose characters
into synthetic line regions to catch them. On the scanned path the OCR
detection boxes serve this role for free.

## Milestones

The scanned path is first on purpose — it avoids the content-stream
interpreter, font parsing, incremental writing and formula detection entirely,
while exercising the IR, translation, typesetting and CLI that both paths
share. It is also the capability BabelDOC lacks.

**1 — Scanned path.** render → layout → OCR → translate → rebuild.
`PDFium render` → `RT-DETR` → `PP-OCRv6` → LLM → new PDF: page image, filled
rectangles, translated text.

**2 — Native path, read side.** Two-pass parse into the IR. Snapshot-test
against `tests/corpus/`.

**3 — Native path, write side.** Incremental editing, font subsetting, CID font
construction.

**4 — Quality.** Formula detection, style merging, CJK line-break prohibition
rules, the long tail of malformed files.

## Testing

`tests/corpus/` holds 23 PDFs, each targeting one failure mode — Type3 fonts,
Identity-H without a ToUnicode CMap, nested Form XObjects, `/Rotate`, CropBox ≠
MediaBox, arXiv line numbers, vertical CJK, borderless tables, pure scans,
invisible OCR layers, null `/Contents`, a missing MediaBox, a lying `/Filter`.
See `tests/corpus/MANIFEST.md` for what each one asserts.

Regenerate or extend them with `tests/gen_corpus.py` (pymupdf) and
`tests/gen_pathological.py` (hand-assembled PDF bytes).

Because the IR is serialisable, the cheapest regression harness is a snapshot
of it after every stage — `insta` does this natively. Worth doing from day one:
BabelDOC has 933 lines of tests for 78k lines of code and no PDF regression
coverage at all, which is why it carries 354 `catch` blocks and twelve
dedicated repair functions.

## Models

Not vendored. Fetch into `models/`:

- Layout — PP-DocLayout (RT-DETR). Export via `paddle2onnx`.
- OCR — official ONNX on the Hugging Face Hub, no conversion needed:
  `PaddlePaddle/PP-OCRv6_{tiny,small,medium}_{det,rec}_onnx`

## Licence

MIT OR Apache-2.0.
