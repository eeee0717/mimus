"""Generate a PDF stress corpus for a BabelDOC-style translation pipeline.

Each file targets a specific failure mode observed in BabelDOC's defensive code
(the fix_* helpers, the layout_priority table, the ocr_workaround branch).
"""

import zlib
from pathlib import Path

import pymupdf

OUT = Path(__file__).parent / "corpus"
OUT.mkdir(exist_ok=True)

W, H = 595.0, 842.0
SERIF, BOLD, ITAL, MONO = "tiro", "tibo", "tiit", "cour"
LOREM = (
    "The intermediate representation lowers a page of drawing operators into a "
    "structure that carries paragraphs, formulas and styles. Each character "
    "retains its own bounding box, font identity and render order so that the "
    "typesetter can place translated glyphs without losing the original layout."
)


def new(doc, rot=0):
    p = doc.new_page(width=W, height=H)
    if rot:
        p.set_rotation(rot)
    return p


def flow(page, rect, text, size=10.5, font=SERIF, leading=1.35):
    """Naive greedy line filler -- gives us real per-line text objects."""
    y = rect.y0 + size
    line, words = "", text.split()
    for w in words:
        trial = f"{line} {w}".strip()
        if pymupdf.get_text_length(trial, fontname=font, fontsize=size) > rect.width:
            page.insert_text((rect.x0, y), line, fontname=font, fontsize=size)
            y += size * leading
            line = w
            if y > rect.y1:
                return y
        else:
            line = trial
    if line:
        page.insert_text((rect.x0, y), line, fontname=font, fontsize=size)
        y += size * leading
    return y


def save(doc, name):
    path = OUT / name
    doc.save(path, garbage=0, deflate=True)
    doc.close()
    return path


# ---------------------------------------------------------------- 01 basic
def c01():
    d = pymupdf.open()
    p = new(d)
    p.insert_text((72, 90), "A Study of Intermediate Representations",
                  fontname=BOLD, fontsize=17)
    p.insert_text((72, 112), "Jane Doe, Institute of Document Engineering",
                  fontname=ITAL, fontsize=9.5)
    flow(p, pymupdf.Rect(72, 130, 523, 300), LOREM)
    flow(p, pymupdf.Rect(72, 310, 523, 460), LOREM, size=8, font=MONO)
    p.insert_text((72, 500), "Mixed sizes: ", fontname=SERIF, fontsize=10)
    x = 72 + pymupdf.get_text_length("Mixed sizes: ", fontname=SERIF, fontsize=10)
    for i, s in enumerate((7, 9, 11, 14, 20)):
        t = f"{s}pt "
        p.insert_text((x, 500), t, fontname=SERIF, fontsize=s)
        x += pymupdf.get_text_length(t, fontname=SERIF, fontsize=s)
    return save(d, "01_basic_text.pdf")


# ------------------------------------------------------------- 02 two column
def c02():
    d = pymupdf.open()
    p = new(d)
    p.insert_text((72, 80), "Two Column Layout", fontname=BOLD, fontsize=16)
    for rect in (pymupdf.Rect(72, 100, 288, 760), pymupdf.Rect(307, 100, 523, 760)):
        flow(p, rect, LOREM * 3, size=9.5)
    return save(d, "02_two_column.pdf")


# ----------------------------------------------------------------- 03 formula
def c03():
    d = pymupdf.open()
    p = new(d)
    p.insert_text((72, 80), "Formulas", fontname=BOLD, fontsize=16)
    y = flow(p, pymupdf.Rect(72, 100, 523, 160),
             "Given the quadratic equation below we obtain the roots by")
    # display formula, centred, in a symbol-ish font
    p.insert_text((200, y + 24), "x = (-b +- sqrt(b", fontname="symb", fontsize=13)
    x = 200 + pymupdf.get_text_length("x = (-b +- sqrt(b", fontname="symb", fontsize=13)
    p.insert_text((x, y + 20), "2", fontname="symb", fontsize=8)          # superscript
    p.insert_text((x + 5, y + 24), " - 4ac)) / 2a", fontname="symb", fontsize=13)
    y += 50
    # inline formula inside running text
    p.insert_text((72, y), "where the discriminant ", fontname=SERIF, fontsize=10.5)
    x = 72 + pymupdf.get_text_length("where the discriminant ", fontname=SERIF, fontsize=10.5)
    p.insert_text((x, y), "D = b", fontname="symb", fontsize=10.5)
    x += pymupdf.get_text_length("D = b", fontname="symb", fontsize=10.5)
    p.insert_text((x, y - 4), "2", fontname="symb", fontsize=7)
    p.insert_text((x + 4, y), "-4ac", fontname="symb", fontsize=10.5)
    x += 4 + pymupdf.get_text_length("-4ac", fontname="symb", fontsize=10.5)
    p.insert_text((x, y), " governs the sign, and H", fontname=SERIF, fontsize=10.5)
    x += pymupdf.get_text_length(" governs the sign, and H", fontname=SERIF, fontsize=10.5)
    p.insert_text((x, y + 3), "2", fontname=SERIF, fontsize=7)            # subscript
    p.insert_text((x + 4, y), "O is unrelated.", fontname=SERIF, fontsize=10.5)
    p.insert_text((480, y + 40), "(1)", fontname=SERIF, fontsize=10)      # formula number
    return save(d, "03_formula.pdf")


# ------------------------------------------------------------- 04/05 tables
def _table(p, x0, y0, ruled):
    cols = [x0, x0 + 130, x0 + 250, x0 + 360]
    rows = [y0 + i * 22 for i in range(6)]
    hdr = ("Model", "Params", "Hmean")
    data = [("PP-OCRv6 tiny", "1.5 M", "80.6"), ("PP-OCRv6 small", "7.7 M", "84.1"),
            ("PP-OCRv6 medium", "34.5 M", "86.2"), ("PP-OCRv5 server", "-", "81.6"),
            ("DocLayout-YOLO", "-", "-")]
    if ruled:
        for yy in rows:
            p.draw_line((cols[0], yy), (cols[-1], yy), width=0.6)
        for xx in cols:
            p.draw_line((xx, rows[0]), (xx, rows[-1]), width=0.6)
    for i, t in enumerate(hdr):
        p.insert_text((cols[i] + 5, rows[0] + 15), t, fontname=BOLD, fontsize=9)
    for r, row in enumerate(data, start=1):
        for i, t in enumerate(row):
            p.insert_text((cols[i] + 5, rows[r] + 15), t, fontname=SERIF, fontsize=9)


def c04():
    d = pymupdf.open()
    p = new(d)
    p.insert_text((72, 80), "Table 1: Model comparison", fontname=ITAL, fontsize=9.5)
    _table(p, 72, 92, ruled=True)
    p.insert_text((72, 250), "Note: Hmean measured on the internal benchmark.",
                  fontname=SERIF, fontsize=8)
    flow(p, pymupdf.Rect(72, 280, 523, 500), LOREM)
    return save(d, "04_table_ruled.pdf")


def c05():
    d = pymupdf.open()
    p = new(d)
    p.insert_text((72, 80), "Table 2: Borderless variant", fontname=ITAL, fontsize=9.5)
    _table(p, 72, 92, ruled=False)
    flow(p, pymupdf.Rect(72, 260, 523, 480), LOREM)
    return save(d, "05_table_borderless.pdf")


# ------------------------------------------------------------ 06 figure+caption
def c06():
    d = pymupdf.open()
    p = new(d)
    flow(p, pymupdf.Rect(72, 80, 523, 200), LOREM)
    box = pymupdf.Rect(150, 230, 445, 430)
    p.draw_rect(box, color=(0.35, 0.35, 0.35), fill=(0.94, 0.94, 0.97), width=0.8)
    for i in range(6):  # a fake bar chart -> vector paths inside a figure region
        h = 20 + i * 25
        p.draw_rect(pymupdf.Rect(175 + i * 45, 410 - h, 205 + i * 45, 410),
                    fill=(0.2, 0.45 + i * 0.08, 0.8), width=0)
    p.insert_text((150, 448), "Figure 1: Throughput across model tiers.",
                  fontname=ITAL, fontsize=9)
    flow(p, pymupdf.Rect(72, 470, 523, 700), LOREM)
    return save(d, "06_figure_caption.pdf")


# ---------------------------------------------------- 07 header/footer, 3 pages
def c07():
    d = pymupdf.open()
    for n in range(1, 4):
        p = new(d)
        p.insert_text((72, 45), "Journal of Document Engineering, Vol. 12",
                      fontname=ITAL, fontsize=8, color=(0.4, 0.4, 0.4))
        p.draw_line((72, 52), (523, 52), width=0.4, color=(0.6, 0.6, 0.6))
        flow(p, pymupdf.Rect(72, 70, 523, 740), LOREM * 4)
        p.draw_line((72, 780), (523, 780), width=0.4, color=(0.6, 0.6, 0.6))
        p.insert_text((290, 795), f"- {n} -", fontname=SERIF, fontsize=8.5)
        p.insert_text((72, 795), "Preprint", fontname=SERIF, fontsize=8,
                      color=(0.4, 0.4, 0.4))
    return save(d, "07_header_footer.pdf")


# ------------------------------------------------------------ 08 arXiv line nums
def c08():
    d = pymupdf.open()
    p = new(d)
    p.insert_text((72, 80), "Manuscript With Line Numbers", fontname=BOLD, fontsize=15)
    y, size, n = 110, 10.0, 1
    words, line = (LOREM * 4).split(), ""
    for w in words:
        trial = f"{line} {w}".strip()
        if pymupdf.get_text_length(trial, fontname=SERIF, fontsize=size) > 420:
            p.insert_text((58, y), str(n), fontname=SERIF, fontsize=7,
                          color=(0.45, 0.45, 0.45))
            p.insert_text((80, y), line, fontname=SERIF, fontsize=size)
            y += size * 1.55
            n += 1
            line = w
            if y > 750:
                break
        else:
            line = trial
    return save(d, "08_line_numbers.pdf")


# -------------------------------------------------------- 09 footnotes + refs
def c09():
    d = pymupdf.open()
    p = new(d)
    y = flow(p, pymupdf.Rect(72, 80, 523, 300), LOREM)
    p.insert_text((72, y + 4), "Prior work established the baseline",
                  fontname=SERIF, fontsize=10.5)
    x = 72 + pymupdf.get_text_length("Prior work established the baseline",
                                     fontname=SERIF, fontsize=10.5)
    p.insert_text((x, y), "1", fontname=SERIF, fontsize=6.5)   # footnote marker
    p.insert_text((x + 4, y + 4), " for this task [3, 7].", fontname=SERIF, fontsize=10.5)
    p.draw_line((72, 690), (220, 690), width=0.5)
    p.insert_text((72, 703), "1", fontname=SERIF, fontsize=6)
    p.insert_text((77, 706), "See the supplementary material for details.",
                  fontname=SERIF, fontsize=8)
    p.insert_text((72, 735), "References", fontname=BOLD, fontsize=10)
    for i, r in enumerate([
        "[3] A. Author. Layout analysis revisited. In Proc. DOC, 2025.",
        "[7] B. Author and C. Author. Neural typesetting. JDE, 12(3), 2026.",
    ]):
        p.insert_text((72, 752 + i * 13), r, fontname=SERIF, fontsize=8)
    return save(d, "09_footnote_refs.pdf")


# ------------------------------------------------------------- 10/11 CJK
CJK = ("中间表示把一页绘图指令降解成携带段落、公式与样式的结构。"
       "每个字符保留自己的包围盒、字体标识与绘制顺序，"
       "以便排版器在不丢失原始版式的前提下放置译文字形。")


def c10():
    d = pymupdf.open()
    p = new(d)
    p.insert_text((72, 90), "中间表示的一项研究", fontname="china-ss", fontsize=18)
    y, size = 125, 11.0
    per = int(430 / size)
    for i in range(0, len(CJK) * 3, per):
        chunk = (CJK * 3)[i:i + per]
        p.insert_text((72, y), chunk, fontname="china-ss", fontsize=size)
        y += size * 1.7
        if y > 760:
            break
    return save(d, "10_cjk_horizontal.pdf")


def c11():
    d = pymupdf.open()
    p = new(d)
    size, col_x, y0 = 12.0, 500.0, 90.0
    for ch in CJK * 2:
        p.insert_text((col_x, y0), ch, fontname="china-ss", fontsize=size)
        y0 += size * 1.15
        if y0 > 760:
            y0, col_x = 90.0, col_x - size * 1.9
            if col_x < 72:
                break
    return save(d, "11_cjk_vertical.pdf")


# ------------------------------------------------------- 12 rotated / 13 boxes
def c12():
    d = pymupdf.open()
    for rot in (90, 180, 270):
        p = new(d)
        p.insert_text((72, 80), f"Page rotation /Rotate {rot}", fontname=BOLD, fontsize=14)
        flow(p, pymupdf.Rect(72, 100, 523, 400), LOREM)
        p.set_rotation(rot)
    return save(d, "12_rotated_pages.pdf")


def c13():
    d = pymupdf.open()
    p = new(d)
    p.insert_text((40, 60), "TRIM MARK - outside cropbox", fontname=SERIF,
                  fontsize=7, color=(0.7, 0.7, 0.7))
    p.insert_text((90, 140), "CropBox differs from MediaBox", fontname=BOLD, fontsize=14)
    flow(p, pymupdf.Rect(90, 160, 500, 500), LOREM)
    p.set_cropbox(pymupdf.Rect(72, 108, 523, 740))
    return save(d, "13_mediabox_cropbox.pdf")


# --------------------------------------------------------- 14 vector + clip
def c14():
    d = pymupdf.open()
    p = new(d)
    p.insert_text((72, 80), "Vector graphics and clipping", fontname=BOLD, fontsize=14)
    sh = p.new_shape()
    sh.draw_bezier((90, 150), (200, 90), (330, 260), (450, 160))
    sh.finish(color=(0.85, 0.15, 0.15), width=1.6)
    sh.draw_circle((260, 330), 70)
    sh.finish(color=(0.1, 0.3, 0.7), fill=(0.85, 0.9, 1.0), width=1.0)
    for i in range(24):  # dense thin strokes -- stresses curve/figure attribution
        sh.draw_line((100 + i * 14, 430), (100 + i * 14, 430 + (i % 7) * 12))
        sh.finish(color=(0.2, 0.2, 0.2), width=0.35)
    sh.commit()
    flow(p, pymupdf.Rect(72, 560, 523, 760), LOREM)
    return save(d, "14_vector_graphics.pdf")


# ------------------------------------------------------------ 15 Form XObject
def c15(src):
    inner = pymupdf.open(src)
    d = pymupdf.open()
    p = new(d)
    p.insert_text((72, 70), "Nested Form XObjects", fontname=BOLD, fontsize=14)
    # show_pdf_page emits a Do on a Form XObject -- two of them, different CTMs
    p.show_pdf_page(pymupdf.Rect(72, 90, 330, 450), inner, 0)
    p.show_pdf_page(pymupdf.Rect(350, 90, 523, 330), inner, 0, rotate=15)
    flow(p, pymupdf.Rect(72, 480, 523, 700), LOREM)
    inner.close()
    return save(d, "15_xobject_form.pdf")


# --------------------------------------------------- 16/17 scanned & OCR layer
def c16(src):
    s = pymupdf.open(src)
    d = pymupdf.open()
    for i in range(s.page_count):
        pix = s[i].get_pixmap(dpi=150)
        p = new(d)
        p.insert_image(pymupdf.Rect(0, 0, W, H), pixmap=pix)
    s.close()
    return save(d, "16_scanned_image_only.pdf")


def c17(src):
    s = pymupdf.open(src)
    d = pymupdf.open()
    page = s[0]
    pix = page.get_pixmap(dpi=150)
    p = new(d)
    p.insert_image(pymupdf.Rect(0, 0, W, H), pixmap=pix)
    # invisible text layer (Tr 3) -- exactly what --ocr-workaround expects
    for blk in page.get_text("dict")["blocks"]:
        for ln in blk.get("lines", []):
            for sp in ln["spans"]:
                p.insert_text((sp["origin"][0], sp["origin"][1]), sp["text"],
                              fontname=SERIF, fontsize=sp["size"], render_mode=3)
    s.close()
    return save(d, "17_scanned_ocr_layer.pdf")


# -------------------------------------------------------------- 18 kitchen sink
def c18():
    d = pymupdf.open()
    p = new(d)
    p.insert_text((72, 45), "Journal of Document Engineering", fontname=ITAL,
                  fontsize=8, color=(0.4, 0.4, 0.4))
    p.insert_text((72, 85), "Everything At Once", fontname=BOLD, fontsize=17)
    p.insert_text((72, 104), "J. Doe, A. Smith", fontname=ITAL, fontsize=9)
    left, right = pymupdf.Rect(72, 120, 288, 620), pymupdf.Rect(307, 120, 523, 480)
    flow(p, left, LOREM * 2, size=9)
    y = flow(p, right, LOREM, size=9)
    p.insert_text((330, y + 20), "E = mc", fontname="symb", fontsize=12)
    p.insert_text((330 + pymupdf.get_text_length("E = mc", fontname="symb", fontsize=12),
                   y + 16), "2", fontname="symb", fontsize=8)
    p.insert_text((500, y + 20), "(1)", fontname=SERIF, fontsize=9)
    _table(p, 307, y + 40, ruled=True)
    p.insert_text((307, y + 175), "Table 1: Results.", fontname=ITAL, fontsize=8)
    box = pymupdf.Rect(307, y + 190, 523, y + 300)
    p.draw_rect(box, color=(0.4, 0.4, 0.4), fill=(0.95, 0.95, 0.98), width=0.7)
    p.insert_text((307, y + 315), "Figure 1: Overview.", fontname=ITAL, fontsize=8)
    p.insert_text((290, 795), "- 1 -", fontname=SERIF, fontsize=8.5)
    return save(d, "18_kitchen_sink.pdf")


for fn in (c01, c02, c03, c04, c05, c06, c07, c08, c09, c10, c11, c12, c13, c14, c18):
    print("ok", fn().name)
base = OUT / "18_kitchen_sink.pdf"
print("ok", c15(base).name)
print("ok", c16(OUT / "07_header_footer.pdf").name)
print("ok", c17(base).name)
