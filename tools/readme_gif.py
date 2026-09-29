#!/usr/bin/env python3
# Renders the README demo GIF: headless-dump colored cells, Pillow raster, ffmpeg palette, gifsicle.

import argparse
import os
import subprocess
import sys
import tempfile

from PIL import Image, ImageDraw, ImageFont

FONT = "/System/Library/Fonts/Menlo.ttc"


def cells(args):
    count = args.end - args.start + 1
    cmd = ["cargo", "run", "--quiet", "--release", "-p", "auto-ascii", "--example", "headless-dump", "--",
           args.asset, str(count), args.grid, "--palette", "ascii", "--settings", args.settings,
           "--from", str(args.start), "--warm", str(args.warm), "--cells"]
    proc = subprocess.Popen(cmd, stdout=subprocess.PIPE, text=True)
    head = proc.stdout.readline()
    while head:
        frame = int(head.split()[2].split("/")[0])
        cols, rows = map(int, head.split()[4].split("x"))
        grid = []
        for _ in range(rows):
            line = proc.stdout.readline().rstrip("\n")
            row = []
            for i in range(cols):
                t = line[i * 20:(i + 1) * 20]
                bg = None if t[14:] == "------" else tuple(bytes.fromhex(t[14:]))
                row.append((chr(int(t[:8], 16)), tuple(bytes.fromhex(t[8:14])), bg))
            grid.append(row)
        yield frame, grid
        head = proc.stdout.readline()
    if proc.wait() != 0:
        sys.exit(f"readme_gif: headless-dump exited {proc.returncode}")


def raster(grid, font, cw, ch, asc, trim):
    rows = grid[trim:len(grid) - trim]
    img = Image.new("RGB", (len(rows[0]) * cw, len(rows) * ch), (0, 0, 0))
    d = ImageDraw.Draw(img)
    for y, row in enumerate(rows):
        for x, (c, fg, bg) in enumerate(row):
            if bg is not None:
                d.rectangle([x * cw, y * ch, (x + 1) * cw - 1, (y + 1) * ch - 1], fill=bg)
            if c != " ":
                d.text((x * cw, y * ch + asc), c, font=font, fill=fg)
    return img


def main():
    p = argparse.ArgumentParser(description="Render the README demo GIF from a .ascii asset.")
    p.add_argument("asset")
    p.add_argument("--settings", help="saved player settings (default: <asset stem>.player.toml)")
    p.add_argument("--start", type=int, default=4036, help="first frame (default: the first frame after the 2:14.5 cut)")
    p.add_argument("--end", type=int, default=4339)
    p.add_argument("--step", type=int, default=3)
    p.add_argument("--delay", type=int, default=10, help="GIF frame delay in centiseconds")
    p.add_argument("--warm", type=int, default=0,
                   help="frames rendered before --start to settle hysteresis; keep them inside the shot")
    p.add_argument("--grid", default="120x34")
    p.add_argument("--trim", type=int, default=4)
    p.add_argument("--font-size", type=float, default=13)
    p.add_argument("--lossy", type=int, default=10)
    p.add_argument("--out", default="docs/assets/architect-2m14s.gif")
    args = p.parse_args()
    args.settings = args.settings or os.path.splitext(args.asset)[0] + ".player.toml"

    font = ImageFont.truetype(FONT, args.font_size)
    cw = round(font.getlength("M"))
    ch = 2 * cw
    ascent, descent = font.getmetrics()
    asc = (ch - (ascent + descent)) // 2
    with tempfile.TemporaryDirectory(prefix="readme-gif-") as tmp:
        n = 0
        for frame, grid in cells(args):
            if (frame - args.start) % args.step == 0:
                raster(grid, font, cw, ch, asc, args.trim).save(f"{tmp}/f{n:04d}.png")
                n += 1
        raw = f"{tmp}/raw.gif"
        graph = "split[a][b];[a]palettegen=stats_mode=diff[p];[b][p]paletteuse=dither=none:diff_mode=rectangle"
        subprocess.run(["ffmpeg", "-v", "error", "-y", "-framerate", f"100/{args.delay}", "-i", f"{tmp}/f%04d.png",
                        "-vf", graph, "-loop", "0", raw], check=True)
        subprocess.run(["gifsicle", "-O3", f"--lossy={args.lossy}", "--delay", str(args.delay),
                        "--loopcount=forever", raw, "-o", args.out], check=True)
    print(f"{args.out}: {n} frames, {n * args.delay / 100:.2f} s, {os.path.getsize(args.out)} bytes")


if __name__ == "__main__":
    main()
