# auto-ascii for agents

`auto-ascii` distills video into `.ascii` assets — resolution-independent
feature files that play as ASCII art in any terminal — kept in one folder you
can list, trim and stitch. `--json` makes stdout one JSON value and errors
`{"error": "..."}` on stderr, exit 1; `play` is interactive and refuses it.

## Home folder

`~/auto-ascii`, or `$AUTO_ASCII_HOME`. `auto-ascii home` prints and creates it.

    library/        <name>.ascii clips, each with a <name>.json sidecar
    compositions/   <name>.toml timelines
    exports/        flattened compositions

## The loop

1. **import** — `auto-ascii import ~/Desktop/clip.mp4` ffmpeg-ingests into
   `library/clip.ascii` (needs `ffmpeg` on PATH). Flags `--name N`, `--ss T`,
   `--t T`, `--fps N`, `--res WxH`, `--force`; times are `SS`/`MM:SS`/`HH:MM:SS`.
2. **list / info** — `auto-ascii list`, `auto-ascii info <clip>`. A `<clip>`
   is a path if one exists, else `library/<clip>.ascii`.
3. **cut / compose** — trim one clip, or stitch many (schema below). *Not in
   this build: these land with M8; a `.toml` written today stays valid.*
4. **play** — `auto-ascii play <clip>`: `q` quits, `?` lists the keys.

## Composition schema

A TOML file that IS the source of truth. File order; gaps black; later clip on top.

```toml
schema = 1
name = "demo"          # optional; defaults to the file stem
[[clip]]
asset = "apple-1984"   # library name, or a path (absolute or relative to this file)
in = "0:05"            # optional trim start inside the asset (default: its start)
out = "0:20"           # optional trim end inside the asset (default: its end)
at = "0:00"            # optional timeline position (default: end of previous clip)
```

## JSON shapes

`import` prints the sidecar it wrote; `list` prints an array of it, with
`source`/`created_unix`/`created` null when a clip has no sidecar. A clip
that will not read still gets a row: `asset` null plus an `error` string.

```json
{"name": "clip", "source": {"path": "/abs/clip.mp4", "sha256": "…", "bytes": 91234},
 "asset": {"path": "/abs/library/clip.ascii", "bytes": 40960, "frames": 360, "fps": 30.0,
           "duration_secs": 12.0, "base_w": 480, "base_h": 270},
 "created_unix": 1758326400, "created": "2025-09-20T00:00:00Z"}
```

## Two rules agents get wrong

1. **Names are kebab-case.** `--name` and the file stem are lowercased with
   runs of non-alphanumerics collapsed to `-`: `My Clip (2).mp4` -> `my-clip-2`.
2. **`at` places, `in`/`out` trim — all three optional.** `in`/`out` are
   positions *inside the asset*; `at` is a position *on the timeline*.
