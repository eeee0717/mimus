#!/usr/bin/env python3
"""Build deterministic cmap-rich test fonts from the OFL Noto Sans SC fixtures."""

from argparse import ArgumentParser
from copy import deepcopy
from pathlib import Path

from fontTools import subset
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


def build(source: Path, output: Path, family: str, characters: str | None) -> None:
    font = TTFont(source, recalcBBoxes=False, recalcTimestamp=False)
    best_cmap = font.getBestCmap()
    donor_glyphs = [best_cmap[ord(value)] for value in "中文测试MIMUS "]
    if characters is None:
        selected_characters = [chr(value) for value in range(0x20, 0x7F)]
        selected_characters.extend(gb2312_level_one())
        selected_characters.extend("，。！？：“”‘’（）《》【】；、—…·")
    else:
        selected_characters = list(characters)

    glyph_order = font.getGlyphOrder()
    glyf = font["glyf"]
    hmtx = font["hmtx"]
    vmtx = font["vmtx"] if "vmtx" in font else None
    assigned: dict[int, str] = {}
    for index, character in enumerate(dict.fromkeys(selected_characters)):
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


def build_variable_subset(source: Path, output: Path, characters: str) -> None:
    unicodes = ",".join(f"U+{ord(value):04X}" for value in dict.fromkeys(characters))
    output.parent.mkdir(parents=True, exist_ok=True)
    result = subset.main(
        [
            str(source),
            f"--output-file={output}",
            f"--unicodes={unicodes}",
            "--layout-features=",
            "--no-hinting",
            "--glyph-names",
            "--legacy-cmap",
            "--no-symbol-cmap",
            "--notdef-glyph",
            "--notdef-outline",
            "--recommended-glyphs",
            "--name-IDs=*",
            "--name-legacy",
            "--name-languages=*",
            "--no-harfbuzz-repacker",
            "--no-recalc-timestamp",
            "--canonical-order",
        ]
    )
    if result not in (None, 0):
        raise RuntimeError(f"fontTools subset failed with exit status {result}")


def main() -> None:
    parser = ArgumentParser()
    parser.add_argument("source", type=Path)
    parser.add_argument("output", type=Path)
    parser.add_argument("family", nargs="?")
    parser.add_argument("--characters")
    parser.add_argument("--variable-subset", action="store_true")
    arguments = parser.parse_args()
    if arguments.variable_subset:
        if arguments.characters is None:
            parser.error("--variable-subset requires --characters")
        build_variable_subset(arguments.source, arguments.output, arguments.characters)
    else:
        if arguments.family is None:
            parser.error("family is required unless --variable-subset is used")
        build(arguments.source, arguments.output, arguments.family, arguments.characters)


if __name__ == "__main__":
    main()
