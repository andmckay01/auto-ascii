# auto-ascii for agents

`auto-ascii` distills video into `.ascii` assets — feature files that play as
ASCII art in any terminal — kept in one folder you can list, trim and stitch.
`--json` makes stdout one JSON value; every error, a mistyped command line
included, is `{"error": "..."}` on stderr with exit 1.

## Home folder

`~/auto-ascii`, or `$AUTO_ASCII_HOME`; `auto-ascii home` prints and creates it,
with `library/` (the `<name>.ascii` clips, each with a `<name>.json` sidecar),
`compositions/` (`<name>.toml` timelines) and `exports/` (flattened ones) in it.

## The loop

1. **import** — `auto-ascii import ~/Desktop/clip.mp4` ffmpeg-ingests into
   `library/clip.ascii` (needs `ffmpeg` on PATH). Flags `--name N`, `--ss T`,
   `--t T`, `--fps N`, `--res WxH`, `--force`; times are `SS`/`MM:SS`/`HH:MM:SS`.
   `--force` replaces a clip only if the rebuild succeeds.
2. **list / info** — `auto-ascii list`, `auto-ascii info <clip>`. A `<clip>`
   is a path if one exists, else `library/<clip>.ascii`.
3. **cut** — `auto-ascii cut <clip> --in T --out T [--name N] [--force]` slices
   one clip into a new one (default name `<clip>-0m05s-0m20s`, nothing re-encoded).
4. **compose** — `compose new <name>` starts a timeline, `compose add <name>
   <clip> [--in T] [--out T] [--at T]` appends one clip, `compose show <name>`
   prints the resolved timeline with gaps and overlaps; `<name>` may be a path.
5. **play / export** — `auto-ascii play <clip | composition>` and `compose play
   <name>` are interactive, so they refuse `--json`: `q` quits and `v` shows the
   controls; clips switch at their boundaries with no re-encode. A bare name means
   the library clip first, so pass the `.toml` path to force a composition.
   `compose export <name> [-o path]` flattens one into `exports/<name>.ascii`.

## Composition schema

A TOML file that IS the source of truth — write it yourself if you prefer,
since `compose add` only appends. File order; gaps black; later clip on top.
Below: `intro` plays 0:00–0:10, black 0:10–0:15, `apple-1984` 0:15–0:20.

```toml
schema = 1
name = "demo"           # optional; defaults to the file stem
[[clip]]
asset = "intro"         # library name, or a path (absolute or relative to this file)
out = "0:10"            # optional trim end inside the asset (default: its end)
[[clip]]
asset = "apple-1984"
in = "0:05"             # optional trim start inside the asset (default: its start)
out = "0:10"
at = "0:15"             # optional timeline position (default: end of previous clip)
```

## JSON shapes

`import` and `cut` print the sidecar they wrote; `list` prints an array of it,
with `source`/`created_unix`/`created` null when there is none, and a row with
`asset` null plus `error` when a clip will not read. A cut's `source` is its
slice; `compose show` prints the timeline (`overlaps` adds `under`/`over`).

```json
{"name": "clip", "source": {"path": "/abs/clip.mp4", "sha256": "…", "bytes": 91234},
 "asset": {"path": "/abs/library/clip.ascii", "bytes": 40960, "frames": 360, "fps": 30.0,
           "duration_secs": 12.0, "base_w": 480, "base_h": 270},
 "created_unix": 1758326400, "created": "2025-09-20T00:00:00Z"}

{"kind": "cut", "from": "clip", "in": 5.0, "out": 20.0}

{"name": "demo", "fps": 30.0, "duration_secs": 20.0, "frame_count": 600,
 "clips": [{"index": 0, "asset": "intro", "path": "/abs/library/intro.ascii",
            "in_secs": 0.0, "out_secs": 10.0, "at_secs": null, "start_secs": 0.0,
            "end_secs": 10.0, "fps": 30.0}],
 "gaps": [{"start_secs": 10.0, "end_secs": 15.0}], "overlaps": []}
```

## Two rules agents get wrong

1. **Names are kebab-case.** `--name` and the file stem are lowercased with
   runs of non-alphanumerics collapsed to `-`: `My Clip (2).mp4` -> `my-clip-2`.
2. **`at` places, `in`/`out` trim — all three optional.** `in`/`out` are
   positions *inside the asset*; `at` is a position *on the timeline*.
