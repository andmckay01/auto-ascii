#!/usr/bin/env bash
# Play an .ascii asset, restarting the player each time the clip reaches its
# end. Usage:
#   play-with-sound.command PLAYER ASSET [PLAYER_ARGS...]
# The player plays the asset's soundtrack itself (a <stem>.m4a/.mp4 or the
# folder's source.mp4 beside it), in sync through scrubs, jumps, pauses and
# loops, and `m` toggles it; nothing else is started here. A third argument
# that is not a flag is the old AUDIO argument: it is logged and ignored.
# The player exits 0 at the end of the clip, 3 when the viewer quits
# (q, Esc, Ctrl-C) and anything else on an error, a panic or a signal.
# Every exit is logged with its status, duration and stderr to $LOG
# (default ~/Library/Logs/auto-ascii/play-with-sound.log). On an unexpected
# exit the window stays open with the error until Return is pressed.
set -u

if [ $# -lt 2 ]; then
    echo "usage: $0 PLAYER ASSET [PLAYER_ARGS...]" >&2
    exit 2
fi
PLAYER=$1 ASSET=$2
shift 2
LOG=${LOG:-$HOME/Library/Logs/auto-ascii/play-with-sound.log}
QUIT_STATUS=3

mkdir -p "$(dirname "$LOG")"
ERR=$(mktemp "${TMPDIR:-/tmp}/play-with-sound.XXXXXX")
trap 'rm -f "$ERR"' EXIT
trap 'exit 130' INT TERM HUP

log() {
    printf '%s %s\n' "$(date '+%Y-%m-%d %H:%M:%S')" "$*" >> "$LOG"
}

hold() {
    printf '\n%s\nLog: %s\nPress Return to close.\n' "$1" "$LOG"
    read -r _ || true
}

if [ $# -ge 1 ] && [ "${1#-}" = "$1" ]; then
    log "ignoring audio argument $1: the player finds and plays the soundtrack itself"
    shift
fi

if ! "$PLAYER" --help 2>/dev/null | grep -q '3 when the viewer quits'; then
    log "refusing $PLAYER: it does not report quit as exit status $QUIT_STATUS"
    hold "$PLAYER does not report quit as exit status $QUIT_STATUS; rebuild it."
    exit 1
fi

run=0
while :; do
    run=$((run + 1))
    t0=$SECONDS
    "$PLAYER" "$ASSET" "$@" 2>"$ERR"
    rc=$?
    secs=$((SECONDS - t0))
    log "run $run exit=$rc secs=$secs asset=$ASSET"
    if [ -s "$ERR" ]; then
        sed 's/^/    stderr: /' "$ERR" >> "$LOG"
    fi
    case $rc in
        0) ;;
        "$QUIT_STATUS") exit 0 ;;
        *)
            cat "$ERR" >&2
            hold "auto-ascii-player exited with status $rc after ${secs}s (run $run)."
            exit "$rc"
            ;;
    esac
done
