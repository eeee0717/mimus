#!/usr/bin/env python3
"""Build deterministic cmap-rich test fonts from the OFL Noto Sans SC fixtures."""

from argparse import ArgumentParser
from copy import deepcopy
from pathlib import Path

from fontTools.ttLib import TTFont


def gb2312_level_one() -> list[str]:
    values: list[str] = []
    for lead in range(0xB0, 0xD8):
        for trail in range(0xA1, 0xFF):
            try:
                values.append(bytes((lead, trail)).decode("gb2312"))
            except UnicodeDecodeError:
                pass
    return values


def build(source: Path, output: Path, family: str) -> None:
    font = TTFont(source, recalcBBoxes=False, recalcTimestamp=False)
    best_cmap = font.getBestCmap()
    donor_glyphs = [best_cmap[ord(value)] for value in "中文测试MIMUS "]
    characters = [chr(value) for value in range(0x20, 0x7F)]
    characters.extend(gb2312_level_one())
    characters.extend("，。！？：“”‘’（）《》【】；、—…·")

    glyph_order = font.getGlyphOrder()
    glyf = font["glyf"]
    hmtx = font["hmtx"]
    vmtx = font["vmtx"] if "vmtx" in font else None
    assigned: dict[int, str] = {}
    for index, character in enumerate(dict.fromkeys(characters)):
        codepoint = ord(character)
        existing = best_cmap.get(codepoint)
        if existing is not None:
            assigned[codepoint] = existing
            continue
        donor = donor_glyphs[index % len(donor_glyphs)]
        glyph_name = f"uni{codepoint:04X}"
        glyf[glyph_name] = deepcopy(glyf[donor])
        hmtx.metrics[glyph_name] = hmtx.metrics[donor]
        if vmtx is not None:
            vmtx.metrics[glyph_name] = vmtx.metrics[donor]
        glyph_order.append(glyph_name)
        assigned[codepoint] = glyph_name
    font.setGlyphOrder(glyph_order)

    for table in font["cmap"].tables:
        if not table.isUnicode():
            continue
        table.cmap.update(assigned)

    for record in font["name"].names:
        if record.nameID in {1, 4, 6}:
            value = family if record.nameID != 6 else family.replace(" ", "")
            record.string = value.encode(record.getEncoding())

    output.parent.mkdir(parents=True, exist_ok=True)
    font.save(output, reorderTables=True)


def main() -> None:
    parser = ArgumentParser()
    parser.add_argument("source", type=Path)
    parser.add_argument("output", type=Path)
    parser.add_argument("family")
    arguments = parser.parse_args()
    build(arguments.source, arguments.output, arguments.family)


if __name__ == "__main__":
    main()
