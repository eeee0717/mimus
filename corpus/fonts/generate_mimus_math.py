#!/usr/bin/env python3
"""Build the deterministic tall-summation fixture font."""

from argparse import ArgumentParser
from pathlib import Path

from fontTools import subset
from fontTools.ttLib import TTFont


def build(source: Path, output: Path) -> None:
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
    subsetter.populate(unicodes=[0x2211])
    subsetter.subset(font)

    glyph_name = font.getBestCmap()[0x2211]
    glyph = font["glyf"][glyph_name]
    coordinates, end_points, flags = glyph.getCoordinates(font["glyf"])
    for index, (x, y) in enumerate(coordinates):
        coordinates[index] = (x, y * 3)
    glyph.coordinates = coordinates
    glyph.endPtsOfContours = end_points
    glyph.flags = flags
    glyph.recalcBounds(font["glyf"])

    family = "Mimus Tall Summation"
    postscript = "MimusTallSummation"
    for record in font["name"].names:
        if record.nameID in {1, 3, 4, 6}:
            value = postscript if record.nameID == 6 else family
            record.string = value.encode(record.getEncoding())

    output.parent.mkdir(parents=True, exist_ok=True)
    font.save(output, reorderTables=True)


def main() -> None:
    parser = ArgumentParser()
    parser.add_argument("source", type=Path)
    parser.add_argument("output", type=Path)
    arguments = parser.parse_args()
    build(arguments.source, arguments.output)


if __name__ == "__main__":
    main()
