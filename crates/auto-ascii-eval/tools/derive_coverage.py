#!/usr/bin/env python3
"""Derive the conservative built-in glyph ink-coverage table (auto-ascii-eval).

Method (PLAN §3.4: "glyphs rasterized 64×128, coverage"):
  For every printable ASCII glyph (U+0020..U+007E), render the glyph
  white-on-black into a 64×128 px cell with ffmpeg's drawtext filter
  (libfreetype) using DejaVu Sans Mono Book — the de-facto default Linux
  monospace and a mid-pack "conservative" choice for ink coverage
  (per-font tables land at M5, PLAN §9.5).

  Font size 106 px: DejaVu Sans Mono's advance is 1233/2048 em ≈ 0.602 em,
  so a 106 px em gives a ~64 px advance (= cell width) and a
  ~123 px line box (ascent 1901 + descent 483 = 2384/2048 em ≈ 1.164 em),
  i.e. a 1:2 cell aspect matching the engine's default (PLAN §3.2).

  coverage(g) = sum(gray) / (255 · 64 · 128)
  — the mean pixel value integrates fractional (antialiased) ink exactly,
  rather than thresholding. The glyph is centered in the cell; position
  does not affect the integral as long as no ink clips (max ink box at
  106 px is ~77 px tall / ≤64 px wide — it never does).

Output: Rust source lines for coverage.rs, printed to stdout.

This script is a reference for how the committed constants were derived;
it is NOT run by CI (the constants in coverage.rs are the artifact) and
depends only on ffmpeg + the system DejaVu font, not on the corpus.

Usage: python3 derive_coverage.py [fontfile]
"""

import subprocess
import sys
import tempfile
import os

FONT = sys.argv[1] if len(sys.argv) > 1 else \
    "/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf"
CELL_W, CELL_H = 64, 128
FONT_SIZE = 106

def glyph_coverage(ch: str, tmpdir: str) -> float:
    txt = os.path.join(tmpdir, "glyph.txt")
    raw = os.path.join(tmpdir, "glyph.raw")
    with open(txt, "w", encoding="utf-8") as f:
        f.write(ch)
    vf = (
        f"drawtext=fontfile={FONT}:textfile={txt}:fontsize={FONT_SIZE}"
        f":fontcolor=white:x=(w-text_w)/2:y=(h-text_h)/2:expansion=none"
    )
    subprocess.run(
        [
            "ffmpeg", "-hide_banner", "-loglevel", "error", "-y",
            "-f", "lavfi", "-i", f"color=black:s={CELL_W}x{CELL_H}",
            "-vf", vf, "-frames:v", "1",
            "-f", "rawvideo", "-pix_fmt", "gray", raw,
        ],
        check=True,
    )
    data = open(raw, "rb").read()
    assert len(data) == CELL_W * CELL_H, (ch, len(data))
    return sum(data) / (255.0 * CELL_W * CELL_H)

def main() -> None:
    with tempfile.TemporaryDirectory() as tmpdir:
        for cp in range(0x20, 0x7F):
            ch = chr(cp)
            cov = glyph_coverage(ch, tmpdir)
            lit = {"'": "\\'", "\\": "\\\\"}.get(ch, ch)
            print(f"    ('{lit}', {cov:.4f}),")

if __name__ == "__main__":
    main()
