#!/usr/bin/env python3
"""Cuts the UI's font files from the Archivo and JetBrains Mono variable fonts (both OFL).

Slint picks a font by family and weight and can't set a variable font's axes, so each cut
is a static instance under its own family name: Archivo (the text), Archivo Condensed (the
catalogue headlines: width 62, weight 860, as on amazynk.com), Archivo Expanded (the
AMAZYNK mark: width 125) and JetBrains Mono (part numbers and other metadata). Each is cut
down to Latin with the punctuation and arrows the UI uses; anything else falls back to a
system font.

Run: uv run --with fonttools python3 crates/suite-ui-markup/tools/make-fonts.py \
         Archivo[wdth,wght].ttf JetBrainsMono[wght].ttf
(the two files are in github.com/google/fonts under ofl/archivo and ofl/jetbrainsmono)
"""
import pathlib
import sys

from fontTools import subset
from fontTools.ttLib import TTFont
from fontTools.varLib.instancer import instantiateVariableFont

OUT = pathlib.Path(__file__).resolve().parent.parent / "ui" / "fonts"

# Latin, Latin-1, Latin Extended-A, and the punctuation, arrows and signs the UI draws.
UNICODES = [
    *range(0x20, 0x7F), *range(0xA0, 0x180), *range(0x2010, 0x2028), *range(0x2030, 0x203B),
    0x20AC, 0x2122, 0x2190, 0x2191, 0x2192, 0x2193, 0x2212, 0x2022,
]

# (source index, axes, family, style, weight class, width class, file)
CUTS = [
    (0, {"wdth": 100, "wght": 400}, "Archivo", "Regular", 400, 5, "archivo/Archivo-Regular.ttf"),
    (0, {"wdth": 100, "wght": 500}, "Archivo", "Medium", 500, 5, "archivo/Archivo-Medium.ttf"),
    (0, {"wdth": 100, "wght": 600}, "Archivo", "SemiBold", 600, 5, "archivo/Archivo-SemiBold.ttf"),
    (0, {"wdth": 62, "wght": 860}, "Archivo Condensed", "ExtraBold", 800, 5, "archivo/ArchivoCondensed-ExtraBold.ttf"),
    (0, {"wdth": 125, "wght": 800}, "Archivo Expanded", "ExtraBold", 800, 5, "archivo/ArchivoExpanded-ExtraBold.ttf"),
    (1, {"wght": 400}, "JetBrains Mono", "Regular", 400, 5, "jetbrains-mono/JetBrainsMono-Regular.ttf"),
    (1, {"wght": 500}, "JetBrains Mono", "Medium", 500, 5, "jetbrains-mono/JetBrainsMono-Medium.ttf"),
]


def rename(font, family, style):
    name = font["name"]
    ribbi = style in ("Regular", "Bold")
    full = f"{family} {style}"
    values = {
        1: family if ribbi else full,
        2: style if ribbi else "Regular",
        3: f"{full}; cut for the amazynk installer",
        4: full,
        6: full.replace(" ", ""),
        16: family,
        17: style,
    }
    for record in list(name.names):
        if record.nameID in (1, 2, 3, 4, 6, 16, 17, 21, 22, 25):
            name.removeNames(nameID=record.nameID)
    for name_id, value in values.items():
        name.setName(value, name_id, 3, 1, 0x409)
        name.setName(value, name_id, 1, 0, 0)


def main():
    sources = sys.argv[1:3]
    if len(sources) != 2:
        sys.exit(__doc__)
    for index, axes, family, style, weight, width, file in CUTS:
        font = instantiateVariableFont(TTFont(sources[index]), axes)
        options = subset.Options(layout_features=["*"], name_IDs=["*"], notdef_outline=True)
        cutter = subset.Subsetter(options)
        cutter.populate(unicodes=UNICODES)
        cutter.subset(font)
        rename(font, family, style)
        font["OS/2"].usWeightClass = weight
        font["OS/2"].usWidthClass = width
        if "STAT" in font:
            del font["STAT"]
        if "DSIG" in font:
            del font["DSIG"]
        path = OUT / file
        path.parent.mkdir(parents=True, exist_ok=True)
        font.save(path)
        print(f"{path.relative_to(OUT)}  {path.stat().st_size // 1024} KB")


if __name__ == "__main__":
    main()
