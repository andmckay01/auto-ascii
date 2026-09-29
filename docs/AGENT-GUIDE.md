# auto-ascii for agents

`auto-ascii` distills video into `.ascii` assets — feature files that play as ASCII
art in any terminal — kept in one folder you can list, trim and stitch. `--json`
makes stdout one JSON value; every error is `{"error": "..."}` on stderr, exit 1.

## Home folder

`~/auto-ascii`, or `$AUTO_ASCII_HOME`; `auto-ascii home` prints and creates it,
with `library/` (the `<name>.ascii` clips, each with a `<name>.json` sidecar),
`compositions/` (`<name>.toml` timelines) and `exports/` (flattened ones) in it.

## The loop

1. **import** — `auto-ascii import ~/Desktop/clip.mp4` ingests into `library/clip.ascii`
   (`-o out.ascii` writes just that file). Flags `--name N`, `--ss T`, `--t T`, `--fps N`,
   `--res WxH`, `--params F`, `--force` (replaces only if the rebuild succeeds); times `SS`/`MM:SS`.
2. **list / info** — `list`; `info <clip>`, where `<clip>` is a path, else `library/<clip>.ascii`.
3. **cut** — `auto-ascii cut <clip> --in T --out T [--name N] [--force]` slices
   one clip into a new one (default name `<clip>-0m05s-0m20s`, nothing re-encoded).
4. **compose** — `compose new <name>` starts a timeline, `compose add <name> <clip> [--in T] [--out T]
   [--at T]` appends one clip, `compose show <name>` prints the timeline with gaps and overlaps.
5. **play / export** — `auto-ascii play <clip | composition>` and `compose play <name>` are
   interactive (no `--json`; exit 0 at the end, 3 on quit): `q` quits, `v` hides the controls, `/`
   style (default `ascii`), `s` saves dials + style as `<clip>.player.toml`. `--sim COLSxROWS:NFRAMES`
   runs headless and prints one JSON stats line; `--help-all` shows every flag. A bare name is the
   library clip first (a `.toml` path is a composition); `compose export <name> [-o path]` flattens.
6. **stream** — `auto-ascii stream <youtube-url | search terms>` plays the first
   video live, with sound, saving nothing (needs `yt-dlp` + `ffmpeg`). Interactive
   like `play`; `--sim COLSxROWS:SECS` runs it headless and prints one JSON line.
7. **tools** — `import` needs ffmpeg + ffprobe, `stream` yt-dlp + ffmpeg (`AUTO_ASCII_FFMPEG`,
   `_FFPROBE`, `_YTDLP`, then PATH, then a cache). A missing one downloads after a stderr
   prompt: pass `--yes` / `AUTO_ASCII_YES=1` or `--json` fails. `doctor [--fetch]` checks/fetches.
   `auto-ascii dev …` (inspect, params, eval, sweep, font-table, sim, bench-seek) is for developers.

## Composition schema

The TOML file IS the source of truth (`compose add` only appends). File order;
gaps black; later clip on top. `intro` plays 0:00–0:10, black, then `apple-1984`.

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

`import`/`cut` print the sidecar they wrote; `list` an array of it (`source`,
`created_unix`, `created` null when absent; `asset` null plus `error` if unreadable).
A cut's `source` is its slice; `compose show` the timeline (`overlaps` adds `under`/`over`).

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
2. **`at` places, `in`/`out` trim** (all optional): `in`/`out` inside the asset, `at` on the timeline.
