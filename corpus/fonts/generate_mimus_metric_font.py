#!/usr/bin/env python3
"""Build a deterministic small OTF with fixture-specific vertical metrics."""

from argparse import ArgumentParser
from pathlib import Path

from fontTools import subset
from fontTools.ttLib import TTFont


UNICODES = [0x20, 0x43, 0x49, 0x4D, 0x53, 0x55]


def build(
    source: Path,
    output: Path,
    family: str,
    postscript: str,
    ascent: int,
    descent: int,
) -> None:
    font = TTFont(source, recalcBBoxes=True, recalcTimestamp=False)
    options = subset.Options(
        canonical_order=True,
        glyph_names=True,
        hinting=False,
        layout_features=[],
        name_IDs=[0, 1, 2, 3, 4, 5, 6],
        name_languages=[0x409],
        name_legacy=False,
        notdef_glyph=True,
        notdef_outline=True,
        recalc_timestamp=False,
        recommended_glyphs=True,
        retain_gids=False,
        symbol_cmap=False,
    )
    subsetter = subset.Subsetter(options=options)
    subsetter.populate(unicodes=UNICODES)
    subsetter.subset(font)

    font["hhea"].ascent = ascent
    font["hhea"].descent = descent
    font["OS/2"].sTypoAscender = ascent
    font["OS/2"].sTypoDescender = descent
    font["OS/2"].usWinAscent = ascent
    font["OS/2"].usWinDescent = abs(descent)

    for record in font["name"].names:
        if record.nameID in {1, 3, 4, 6}:
            value = postscript if record.nameID == 6 else family
            record.string = value.encode(record.getEncoding())
    cff = font["CFF "].cff
    cff.fontNames = [postscript]
    top = cff.topDictIndex[0]
    top.FamilyName = family
    top.FullName = family
    top.FontName = postscript

    output.parent.mkdir(parents=True, exist_ok=True)
    font.save(output, reorderTables=True)


def main() -> None:
    parser = ArgumentParser()
    parser.add_argument("source", type=Path)
    parser.add_argument("output", type=Path)
    parser.add_argument("family")
    parser.add_argument("postscript")
    parser.add_argument("ascent", type=int)
    parser.add_argument("descent", type=int)
    arguments = parser.parse_args()
    build(
        arguments.source,
        arguments.output,
        arguments.family,
        arguments.postscript,
        arguments.ascent,
        arguments.descent,
    )


if __name__ == "__main__":
    main()
