#!/usr/bin/env python3
"""WCAG contrast of every text colour against the backgrounds it is drawn on, in both themes.

Run: python3 crates/suite-ui-markup/tools/contrast.py  (exits 1 if a pair is under its minimum)
"""
import pathlib
import re
import sys

THEME = pathlib.Path(__file__).resolve().parent.parent / "ui" / "theme.slint"

# Backgrounds text sits on: the paper, a hovered row or a notice, a hovered control, a dialog.
SURFACES = ["bg", "surface", "raised", "dialog"]

# (text, background, minimum): 4.5 for text, 3.0 for focus rings, icons and controls.
PAIRS = [
    *[(fg, bg, 4.5) for fg in ["text", "soft-text", "muted"] for bg in SURFACES],
    *[(tone, bg, 4.5) for tone in ["accent", "ok", "warn", "bad"] for bg in ["bg", "surface", "dialog"]],
    ("strong", "bg", 4.5),
    # Filled controls: a main button, its hover in signal, a hovered plain or "remove" button.
    ("on-ink", "ink", 4.5),
    ("on-signal", "signal", 4.5),
    ("on-ink", "bad", 4.5),
    # Rules that carry meaning, the gauge, control outlines, the focus ring.
    ("line", "bg", 3.0),
    ("line", "surface", 3.0),
    ("ink", "border", 3.0),
    ("focus", "bg", 3.0),
    ("focus", "surface", 3.0),
    ("focus", "dialog", 3.0),
    # The switch: its knob on the signal track when on, its outline and knob on paper when off.
    ("on-signal", "signal", 3.0),
    ("line", "bg", 3.0),
    # QR codes: dark modules on paper.
    ("qr-dark", "paper", 7.0),
]


def parse():
    src = THEME.read_text()
    dark, light = {}, {}
    for m in re.finditer(r"property <color> ([\w-]+):\s*(?:dark \? (#\w+) : (#\w+)|(#\w+));", src):
        name, d, l, both = m.groups()
        dark[name], light[name] = (d, l) if d else (both, both)
    # crates/suite-ui/src/qr.rs draws modules at 0x10 on white.
    dark["qr-dark"] = light["qr-dark"] = "#101010"
    return dark, light


def rgb(hex_):
    h = hex_.lstrip("#")
    return [int(h[i:i + 2], 16) / 255 for i in (0, 2, 4)]


def luminance(c):
    lin = [v / 12.92 if v <= 0.04045 else ((v + 0.055) / 1.055) ** 2.4 for v in rgb(c)]
    return 0.2126 * lin[0] + 0.7152 * lin[1] + 0.0722 * lin[2]


def ratio(a, b):
    la, lb = sorted([luminance(a), luminance(b)], reverse=True)
    return (la + 0.05) / (lb + 0.05)


def main():
    dark, light = parse()
    failed = False
    print(f"{'text':<14} {'background':<14} {'min':>4} {'dark':>6} {'light':>6}")
    for fg, bg, minimum in PAIRS:
        rd, rl = ratio(dark[fg], dark[bg]), ratio(light[fg], light[bg])
        bad = rd < minimum or rl < minimum
        failed |= bad
        print(f"{fg:<14} {bg:<14} {minimum:>4} {rd:>6.2f} {rl:>6.2f}{'  FAIL' if bad else ''}")
    print("FAILED" if failed else f"all {len(PAIRS)} pairs pass in both themes")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
