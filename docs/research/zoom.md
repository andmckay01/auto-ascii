<!-- Zoom discoverability — can the player change the terminal's font size? 2026-09-25; input to the info-row size readout, the zoom hint and the big overlay text (INTERFACES note 27 (i)) -->

# Can the player zoom the terminal? — digest

**Why it matters.** The asset is resolution-independent: the player picks
glyphs for whatever grid it gets, so a smaller font means more cells and a
sharper picture. The question was whether the player could change the font
size itself, e.g. on up/down arrows, instead of asking the viewer to press
Cmd/Ctrl +/-.

**Answer: no, not across terminals.** No standard escape sequence changes
the font size. Programmatic controls and their requirements vary by terminal:
Alacritty supports runtime configuration over Unix IPC (enabled by default),
and xterm permits OSC 50 when `allowFontOps` is enabled (default true).
Others require configuration or OS permission. An in-app arrow zoom would
therefore need terminal-specific integrations. The player instead suggests
the terminal's own zoom keys; the displayed Cmd/Ctrl shortcut is a hint,
not a binding guaranteed across terminals or user configurations.

| terminal | user zoom keys | can a program inside do it? | needs |
|---|---|---|---|
| Ghostty | Cmd +/-/0 (macOS), Ctrl +/-/0 (Linux): `increase_font_size:N`, `decrease_font_size:N`, `reset_font_size`, `set_font_size:N` [1] | No escape sequence or in-band IPC [1]. macOS: Ghostty ≥ 1.3 has AppleScript, where `perform action` runs keybind action strings, so `perform action "increase_font_size:1"` on a terminal should work (not verified; the docs don't list which actions it accepts) [2] | macOS only; Ghostty ≥ 1.3; the player would shell out to `osascript`; a TCC Automation prompt the first time [2] |
| kitty | Ctrl+Shift +/- | Yes: `kitten @ set-font-size`, also sendable in-band as an `ESC P @kitty-cmd … ESC \` DCS [3] | `allow_remote_control` is `no` by default [4] |
| iTerm2 | Cmd +/- | No font-size code in its proprietary set [5]. `OSC 1337;SetProfile=` switches to a profile the user must have made with a smaller font; `SetProfileProperty` may reach font keys (unverified). The Python API and AppleScript need to be switched on | a user-made profile, or the API enabled |
| xterm | Ctrl+right-click font menu | Yes: `OSC 50 ; #+1 BEL` / `#-1` steps through the font menu [6] | the `allowFontOps` resource, default true [8]; xterm only (urxvt has its own OSC 50) |
| WezTerm | Ctrl/Cmd +/- (`IncreaseFontSize`) | Not directly. `OSC 1337;SetUserVar=` fires `user-var-changed` in the Lua config, which could call `window:set_config_overrides{font_size=…}` (from memory, not checked) | the user writes the Lua handler |
| Alacritty | Ctrl/Cmd +/- | Yes: `alacritty msg config font.size=…` updates runtime configuration [7] | the `alacritty` binary on PATH, Unix IPC (enabled by default) [7] |
| Terminal.app | Cmd +/- | AppleScript `font size` of a tab / settings set (from memory, not checked) | `osascript` plus a TCC Automation prompt |
| Windows Terminal | Ctrl +/-, Ctrl+wheel | No escape sequence; font size lives in `settings.json` (from memory) | — |
| VS Code terminal | Ctrl/Cmd +/- zooms the whole window; `terminal.integrated.fontSize` | No escape sequence; only settings or an extension (from memory) | — |

Other routes, all rejected:

- **Synthetic keystrokes** (`osascript … keystroke "-" using command down`)
  need the macOS Accessibility permission and type into whatever app is in
  front. That is fragile and intrusive.
- **DEC double-height/double-width lines** (`ESC # 3/4/6`) make one line
  bigger rather than the font smaller. xterm supports them; I don't believe
  Ghostty or kitty do (not verified). kitty's text-sizing protocol (OSC 66)
  is kitty-only. Neither changes how many cells the picture gets.
- **`CSI 8 ; rows ; cols t`** resizes the *window* in cells without changing
  the font, and most terminals disable it.

**What shipped instead** (INTERFACES note 27 (i)): the controls overlay
(`v`) prints the grid size (`213x58 cells`). On a narrow grid it adds a row
above that: `Cmd - to zoom out: more cells, a sharper picture` (Ctrl off
macOS). A resize raises the overlay briefly, so zooming shows the new size
as it changes. At 240 or more columns and 36 or more rows the overlay text
is drawn in 3x5 block letters (non-ASCII tiers only), so it is still readable
after zooming out.
Up/Down stay unbound.

The mechanisms marked "should work" or "from memory" were not tried here.
If in-app zoom comes up again, Ghostty's AppleScript `perform action` is
the one to prototype first: the viewer uses Ghostty on macOS, and it needs
no config beyond a single permission prompt.

## Sources

1. Ghostty keybind action reference — https://ghostty.org/docs/config/keybind/reference
2. Ghostty AppleScript (macOS) — https://ghostty.org/docs/features/applescript ; 1.3.0 release notes — https://ghostty.org/docs/install/release-notes/1-3-0
3. kitty remote control — https://sw.kovidgoyal.net/kitty/remote-control/ ; `kitten @ set-font-size` — https://www.mankier.com/1/kitten-@-set-font-size
4. kitty.conf `allow_remote_control` — https://sw.kovidgoyal.net/kitty/conf/
5. iTerm2 proprietary escape codes — https://iterm2.com/documentation-escape-codes.html
6. xterm OSC 50 — https://terminalguide.namepad.de/seq/osc-50_xterm/
7. Alacritty [runtime configuration](https://alacritty.org/cmd-alacritty-msg.html) and [IPC defaults](https://alacritty.org/config-alacritty.html#general)
8. xterm [allowFontOps resource](https://invisible-island.net/xterm/manpage/xterm.html#VT100-Widget-Resources:allowFontOps)
