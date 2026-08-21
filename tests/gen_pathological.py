"""Hand-assembled PDFs for the cases pymupdf will not emit.

These target BabelDOC's fix_null_page_content / fix_null_xref / fix_filter /
fix_media_box / check_cid_char repair paths.
"""

from pathlib import Path

OUT = Path(__file__).parent / "corpus"


class Pdf:
    """Minimal object-graph writer with a correct classic xref table."""

    def __init__(self):
        self.objs = [None]  # 1-based

    def add(self, body: bytes) -> int:
        self.objs.append(body)
        return len(self.objs) - 1

    def stream(self, dic: str, data: bytes) -> int:
        head = f"<<{dic}/Length {len(data)}>>".encode()
        return self.add(head + b"\nstream\n" + data + b"\nendstream")

    def write(self, path: Path, root: int, extra_trailer: str = ""):
        out = bytearray(b"%PDF-1.7\n%\xe2\xe3\xcf\xd3\n")
        offs = [0] * len(self.objs)
        for i, body in enumerate(self.objs):
            if i == 0 or body is None:
                continue
            offs[i] = len(out)
            out += f"{i} 0 obj\n".encode() + body + b"\nendobj\n"
        xref = len(out)
        n = len(self.objs)
        out += f"xref\n0 {n}\n".encode()
        out += b"0000000000 65535 f \n"
        for i in range(1, n):
            out += (f"{offs[i]:010d} 00000 n \n").encode() if offs[i] else b"0000000000 65535 f \n"
        out += f"trailer\n<</Size {n}/Root {root} 0 R{extra_trailer}>>\nstartxref\n{xref}\n%%EOF\n".encode()
        path.write_bytes(out)
        return path


def _shell(p: Pdf, page_body: str, extra_objs=None):
    cat = p.add(b"<</Type/Catalog/Pages 2 0 R>>")
    assert cat == 1
    p.add(b"<</Type/Pages/Kids[3 0 R]/Count 1>>")
    p.add(page_body.encode())
    return p


# ------------------------------------------------------------ 19 Type3 font
def type3():
    p = Pdf()
    p.add(b"<</Type/Catalog/Pages 2 0 R>>")
    p.add(b"<</Type/Pages/Kids[3 0 R]/Count 1>>")
    p.add(b"<</Type/Page/Parent 2 0 R/MediaBox[0 0 595 842]"
          b"/Resources<</Font<</T3 6 0 R/F1 9 0 R>>>>/Contents 4 0 R>>")
    content = (b"BT /F1 13 Tf 72 780 Td (Type3 font below: each glyph is a content stream) Tj ET\n"
               b"BT /T3 36 Tf 72 700 Td (abab) Tj ET\n"
               b"BT /T3 18 Tf 72 650 Td (baba) Tj ET\n"
               b"BT /F1 11 Tf 72 600 Td (Type3 glyphs carry no unicode; a FontMatrix scales them.) Tj ET\n")
    p.stream("", content)  # obj 4
    p.add(b"<</Type/Encoding/Differences[97/square 98/triangle]>>")  # 5
    p.add(b"<</Type/Font/Subtype/Type3/FontBBox[0 0 750 750]"
          b"/FontMatrix[0.001 0 0 0.001 0 0]/CharProcs 8 0 R/Encoding 5 0 R"
          b"/FirstChar 97/LastChar 98/Widths[750 750]/Resources<<>>>>")  # 6
    sq = p.stream("", b"750 0 0 0 750 750 d1\n0 0 750 750 re f\n")      # 7
    p.add(f"<</square {sq} 0 R/triangle 10 0 R>>".encode())              # 8
    p.add(b"<</Type/Font/Subtype/Type1/BaseFont/Helvetica>>")            # 9
    p.stream("", b"750 0 0 0 750 750 d1\n0 0 m 750 0 l 375 750 l f\n")   # 10
    return p.write(OUT / "19_type3_font.pdf", 1)


# --------------------------------------------- 20 Identity-H, no ToUnicode
def no_tounicode():
    p = Pdf()
    p.add(b"<</Type/Catalog/Pages 2 0 R>>")
    p.add(b"<</Type/Pages/Kids[3 0 R]/Count 1>>")
    p.add(b"<</Type/Page/Parent 2 0 R/MediaBox[0 0 595 842]"
          b"/Resources<</Font<</C0 5 0 R/F1 8 0 R>>>>/Contents 4 0 R>>")
    # 2-byte CIDs under Identity-H with NO ToUnicode -> extraction yields CID junk
    cids = b"".join(f"{c:04X}".encode() for c in (36, 82, 79, 72, 3, 55, 72, 91, 87))
    p.stream("", b"BT /F1 12 Tf 72 780 Td (Below: Identity-H CIDs without a ToUnicode CMap) Tj ET\n"
                 b"BT /C0 22 Tf 72 720 Td <" + cids + b"> Tj ET\n"
                 b"BT /F1 10 Tf 72 670 Td (A correct pipeline must detect this and refuse, not emit mojibake.) Tj ET\n")
    p.add(b"<</Type/Font/Subtype/Type0/BaseFont/Arial/Encoding/Identity-H"
          b"/DescendantFonts[6 0 R]>>")                                   # 5
    p.add(b"<</Type/Font/Subtype/CIDFontType2/BaseFont/Arial"
          b"/CIDSystemInfo<</Registry(Adobe)/Ordering(Identity)/Supplement 0>>"
          b"/FontDescriptor 7 0 R/DW 600>>")                              # 6
    p.add(b"<</Type/FontDescriptor/FontName/Arial/Flags 32/FontBBox[-665 -325 2000 1006]"
          b"/ItalicAngle 0/Ascent 905/Descent -212/CapHeight 716/StemV 80>>")  # 7
    p.add(b"<</Type/Font/Subtype/Type1/BaseFont/Helvetica>>")             # 8
    return p.write(OUT / "20_no_tounicode_cid.pdf", 1)


# ----------------------------------------------- 21 null /Contents + null xref
def null_content():
    p = Pdf()
    p.add(b"<</Type/Catalog/Pages 2 0 R>>")
    p.add(b"<</Type/Pages/Kids[3 0 R 5 0 R]/Count 2>>")
    p.add(b"<</Type/Page/Parent 2 0 R/MediaBox[0 0 595 842]/Contents 4 0 R>>")
    p.add(b"null")                                                        # 4: null contents
    p.add(b"<</Type/Page/Parent 2 0 R/MediaBox[0 0 595 842]"
          b"/Resources<</Font<</F1 7 0 R>>>>/Contents 6 0 R>>")           # 5: healthy page
    p.stream("", b"BT /F1 13 Tf 72 780 Td (Page 2 is fine; page 1 has a null /Contents.) Tj ET\n")
    p.add(b"<</Type/Font/Subtype/Type1/BaseFont/Helvetica>>")
    return p.write(OUT / "21_null_contents.pdf", 1)


# ---------------------------------------------------------- 22 missing MediaBox
def missing_mediabox():
    p = Pdf()
    p.add(b"<</Type/Catalog/Pages 2 0 R>>")
    p.add(b"<</Type/Pages/Kids[3 0 R]/Count 1>>")   # no inheritable MediaBox
    p.add(b"<</Type/Page/Parent 2 0 R"
          b"/Resources<</Font<</F1 5 0 R>>>>/Contents 4 0 R>>")
    p.stream("", b"BT /F1 13 Tf 72 700 Td (This page declares no MediaBox anywhere in the tree.) Tj ET\n"
                 b"BT /F1 11 Tf 72 670 Td (Readers must fall back to US Letter or A4.) Tj ET\n")
    p.add(b"<</Type/Font/Subtype/Type1/BaseFont/Helvetica>>")
    return p.write(OUT / "22_missing_mediabox.pdf", 1)


# ------------------------------------------------- 23 wrong /Filter declaration
def bad_filter():
    p = Pdf()
    p.add(b"<</Type/Catalog/Pages 2 0 R>>")
    p.add(b"<</Type/Pages/Kids[3 0 R]/Count 1>>")
    p.add(b"<</Type/Page/Parent 2 0 R/MediaBox[0 0 595 842]"
          b"/Resources<</Font<</F1 5 0 R>>>>/Contents 4 0 R>>")
    raw = b"BT /F1 13 Tf 72 700 Td (Stream claims /FlateDecode but the bytes are plain.) Tj ET\n"
    p.stream("/Filter/FlateDecode", raw)  # lie about the filter
    p.add(b"<</Type/Font/Subtype/Type1/BaseFont/Helvetica>>")
    return p.write(OUT / "23_bad_filter.pdf", 1)


for f in (type3, no_tounicode, null_content, missing_mediabox, bad_filter):
    print("ok", f().name)
