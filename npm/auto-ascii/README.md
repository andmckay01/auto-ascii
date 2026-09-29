# auto-ascii

Realtime ASCII-art video in your terminal. `auto-ascii` imports a video (a
YouTube link or any file ffmpeg reads) into a resolution-independent `.ascii`
asset, then plays it onto whatever cell grid your terminal has right now:
glyph ramps, directional edge strokes and half-blocks, letterboxed, reflowing
live on resize, with sound. It can also stream a video straight to the
terminal without saving anything.

## Install

```bash
npx auto-ascii --help         # try it without installing
npm install -g auto-ascii     # put the auto-ascii command on your PATH
```

## Quickstart

```bash
auto-ascii add "https://youtu.be/jNQXAC9IVRw"   # import a YouTube link into your library
auto-ascii list                                 # what the library holds
auto-ascii play me-at-the-zoo                   # q quits
auto-ascii stream me at the zoo                 # play the first search result live, saving nothing
```

`add` and `stream` need `ffmpeg` and `ffprobe`, and YouTube links need
`yt-dlp`; if one is not on your `PATH`, the CLI offers on first use to
download a checksum-verified build into its cache. `play` needs none of them,
except for sound. On Linux the binary needs ALSA (`libasound2`) at runtime:
`sudo apt install libasound2t64` (or `libasound2`) on Debian/Ubuntu,
`sudo dnf install alsa-lib` on Fedora. `auto-ascii --help` lists the everyday
commands and `--help-all` every command and flag.

## Platforms

This package is a small Node launcher. npm installs exactly one prebuilt
binary alongside it, through an optional dependency matched to your OS and
CPU: macOS (arm64, x64), Linux (x64, arm64) and Windows (x64, arm64). Nothing
runs or downloads at install time. Installing with `--omit=optional` skips
the binary, and the launcher will say so.

For other platforms and other ways to install (a shell installer, Homebrew,
`cargo install`, building from source), see
[github.com/andmckay01/auto-ascii](https://github.com/andmckay01/auto-ascii).

## License

MIT. See the bundled `LICENSE` file.
