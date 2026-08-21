# PDF Stress Corpus

23 files targeting the failure modes encoded in BabelDOC's defensive code
(`fix_null_page_content`, `fix_filter`, `fix_media_box`, `check_cid_char`,
the `layout_priority` table, and the `ocr_workaround` branch).

Regenerate with `gen_corpus.py` (pymupdf) and `gen_pathological.py` (raw bytes).

## A. Parser stress

| file | stresses | a correct pipeline must |
|---|---|---|
| `01_basic_text.pdf` | 4 fonts, 5 sizes, one column | keep per-char font/size; not merge the size ladder into one style |
| `19_type3_font.pdf` | Type3 glyphs (each glyph is a content stream) + FontMatrix | render glyphs, assign no Unicode, never translate them |
| `20_no_tounicode_cid.pdf` | Type0/Identity-H, **no ToUnicode CMap** | detect and refuse — extraction yields `Aole Text` mojibake |
| `15_xobject_form.pdf` | two nested Form XObjects, one rotated 15° | track `xobj_id`, apply nested CTMs |
| `14_vector_graphics.pdf` | bezier, filled circle, 24 thin strokes | not mistake stroke clusters for text or formulas |
| `12_rotated_pages.pdf` | `/Rotate` 90 / 180 / 270 | normalise rotation before layout inference |
| `13_mediabox_cropbox.pdf` | CropBox ⊂ MediaBox, trim mark outside crop | use CropBox; drop the out-of-crop mark |

## B. Layout / paragraph stress

| file | stresses |
|---|---|
| `02_two_column.pdf` | two columns — the classic reading-order trap |
| `03_formula.pdf` | display formula + inline formula + super/subscript + formula number `(1)` |
| `04_table_ruled.pdf` | ruled table (10 vector lines) + caption above |
| `05_table_borderless.pdf` | same data, **no rules** — pure whitespace alignment |
| `06_figure_caption.pdf` | figure region with vector bars + caption below |
| `07_header_footer.pdf` | 3 pages, running header, rule lines, page numbers |
| `08_line_numbers.pdf` | arXiv-style line numbers in the left margin (cf. `merge_alternating_line_number_paragraphs`) |
| `09_footnote_refs.pdf` | footnote marker, separator rule, footnote body, reference list |
| `10_cjk_horizontal.pdf` | CJK horizontal, no inter-word spaces |
| `11_cjk_vertical.pdf` | CJK **vertical** columns, right-to-left |
| `18_kitchen_sink.pdf` | two columns + formula + ruled table + figure + header/footer on one page |

## C. OCR path

| file | stresses |
|---|---|
| `16_scanned_image_only.pdf` | 3 pages, **0 characters**, one image each — the pure scan case |
| `17_scanned_ocr_layer.pdf` | page image + invisible text layer (`Tr 3`) — what `--ocr-workaround` expects |

## D. Malformed input

| file | pathology | observed |
|---|---|---|
| `21_null_contents.pdf` | page 1 `/Contents` resolves to `null` | page 1 empty, page 2 fine |
| `22_missing_mediabox.pdf` | no MediaBox anywhere in the page tree | reader falls back to 612×792 (US Letter) |
| `23_bad_filter.pdf` | stream declares `/FlateDecode`, bytes are plain | `zlib error: incorrect header check`, 0 chars |

## Verified properties

```
file                         pg  chars imgs draws fonts  notes
01_basic_text.pdf             1    707    0     0     4
02_two_column.pdf             1   1772    0     0     2
03_formula.pdf                1    177    0     0     3
04_table_ruled.pdf            1    507    0    10     3
05_table_borderless.pdf       1    461    0     0     3
06_figure_caption.pdf         1    627    0     7     2
07_header_footer.pdf          3   3675    0     6     2
08_line_numbers.pdf           1   1179    0     0     2
09_footnote_refs.pdf          1    538    0     1     2
10_cjk_horizontal.pdf         1    241    0     0     1
11_cjk_vertical.pdf           1    300    0     0     1
12_rotated_pages.pdf          3    956    0     0     2   rot=90,180,270
13_mediabox_cropbox.pdf       1    323    0     0     2   crop!=media
14_vector_graphics.pdf        1    322    0    26     2
15_xobject_form.pdf           1   2598    0    22     4
16_scanned_image_only.pdf     3      0    3     0     0
17_scanned_ocr_layer.pdf      1   1142    1     0     1
18_kitchen_sink.pdf           1   1142    0    11     4
19_type3_font.pdf             1    116    0     0     2   Type3 registered
20_no_tounicode_cid.pdf       1    125    0     0     2   Type0/Identity-H
21_null_contents.pdf          2     45    0     0     1   p1 empty
22_missing_mediabox.pdf       1     96    0     0     1   612x792 fallback
23_bad_filter.pdf             1      0    0     0     1   zlib error
```

## Suggested use as a regression harness

The IL is serialisable, so snapshot-test every pass:

1. run the pipeline with `--skip-translation` (no API key, deterministic)
2. dump the IL after each pass to JSON
3. commit those as golden files
4. diff on every change

In Rust, `insta` does this natively. BabelDOC has 933 lines of tests for 78k lines
of code and zero PDF regression coverage — this is the cheapest place to beat it.
