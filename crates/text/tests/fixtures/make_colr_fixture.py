"""Regenerate ColrFixture.ttf from DejaVuSans-subset.ttf (needs fontTools).

Each color glyph exercises one COLR feature the portable color raster paints:
  O  COLRv0 single red layer
  H  COLRv1 linear gradient, red -> blue, left to right
  R  COLRv1 radial gradient, green center -> blue rim
  S  COLRv1 sweep gradient
  T  COLRv1 composite: solid green SourceIn over a red T -> a green T
  X  COLRv1 translate by +1024 units (half an em) in x
"""

import pathlib

from fontTools.colorLib.builder import buildCOLR, buildCPAL
from fontTools.ttLib import TTFont
from fontTools.ttLib.tables.otTables import CompositeMode, ExtendMode, PaintFormat

here = pathlib.Path(__file__).parent
font = TTFont(here / "DejaVuSans-subset.ttf")

RED, BLUE, GREEN = 0, 1, 2
palette = [(1.0, 0.0, 0.0, 1.0), (0.0, 0.0, 1.0, 1.0), (0.0, 1.0, 0.0, 1.0)]


def solid(index):
    return {"Format": PaintFormat.PaintSolid, "PaletteIndex": index, "Alpha": 1.0}


def line(a, b):
    return {
        "Extend": ExtendMode.PAD,
        "ColorStop": [
            {"StopOffset": 0.0, "PaletteIndex": a, "Alpha": 1.0},
            {"StopOffset": 1.0, "PaletteIndex": b, "Alpha": 1.0},
        ],
    }


def glyph(name, paint):
    return {"Format": PaintFormat.PaintGlyph, "Glyph": name, "Paint": paint}


colr_v0 = {"O": [("O", RED)]}
colr_v1 = {
    "H": glyph(
        "H",
        {
            "Format": PaintFormat.PaintLinearGradient,
            "ColorLine": line(RED, BLUE),
            "x0": 200, "y0": 0, "x1": 1300, "y1": 0, "x2": 200, "y2": 1000,
        },
    ),
    "R": glyph(
        "R",
        {
            "Format": PaintFormat.PaintRadialGradient,
            "ColorLine": line(GREEN, BLUE),
            "x0": 700, "y0": 750, "r0": 0, "x1": 700, "y1": 750, "r1": 900,
        },
    ),
    "S": glyph(
        "S",
        {
            "Format": PaintFormat.PaintSweepGradient,
            "ColorLine": line(RED, BLUE),
            "centerX": 650, "centerY": 750, "startAngle": 0.0, "endAngle": 360.0,
        },
    ),
    "T": {
        "Format": PaintFormat.PaintComposite,
        "SourcePaint": solid(GREEN),
        "CompositeMode": CompositeMode.SRC_IN,
        "BackdropPaint": glyph("T", solid(RED)),
    },
    "X": {
        "Format": PaintFormat.PaintTranslate,
        "dx": 1024, "dy": 0,
        "Paint": glyph("X", solid(BLUE)),
    },
}

font["COLR"] = buildCOLR({**colr_v0, **colr_v1}, glyphMap=font.getReverseGlyphMap())
font["CPAL"] = buildCPAL([palette])
for record in font["name"].names:
    if record.nameID in (1, 3, 4, 6, 16):
        record.string = "VisoColrFixture" if record.nameID == 6 else "Viso COLR Fixture"
font.save(here / "ColrFixture.ttf")
