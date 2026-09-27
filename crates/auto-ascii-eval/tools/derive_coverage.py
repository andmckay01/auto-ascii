#!/usr/bin/env python3

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
