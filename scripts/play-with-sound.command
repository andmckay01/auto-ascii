#!/usr/bin/env bash
# Play an .ascii asset with its soundtrack, restarting both each time the
# clip reaches its end. Usage:
#   play-with-sound.command PLAYER ASSET AUDIO [PLAYER_ARGS...]
# The player exits 0 at the end of the clip, 3 when the viewer quits
# (q, Esc, Ctrl-C) and anything else on an error, a panic or a signal.
# Every exit is logged with its status, duration and stderr to $LOG
# (default ~/Library/Logs/auto-ascii/play-with-sound.log). On an unexpected
# exit the window stays open with the error until Return is pressed.
# The audio restarts with each run; scrubs, jumps and pauses inside a run
# do not move it. $AFPLAY overrides the audio command (default afplay).
set -u

if [ $# -lt 3 ]; then
    echo "usage: $0 PLAYER ASSET AUDIO [PLAYER_ARGS...]" >&2
    exit 2
fi
PLAYER=$1 ASSET=$2 AUDIO=$3
shift 3
AFPLAY=${AFPLAY:-afplay}
LOG=${LOG:-$HOME/Library/Logs/auto-ascii/play-with-sound.log}
QUIT_STATUS=3

mkdir -p "$(dirname "$LOG")"
ERR=$(mktemp "${TMPDIR:-/tmp}/play-with-sound.XXXXXX")
A=
trap 'if [ -n "$A" ]; then kill "$A" 2>/dev/null; fi; rm -f "$ERR"' EXIT
trap 'exit 130' INT TERM HUP

log() {
    printf '%s %s\n' "$(date '+%Y-%m-%d %H:%M:%S')" "$*" >> "$LOG"
}

hold() {
    printf '\n%s\nLog: %s\nPress Return to close.\n' "$1" "$LOG"
    read -r _ || true
}

if ! "$PLAYER" --help 2>/dev/null | grep -q '3 when the viewer quits'; then
    log "refusing $PLAYER: it does not report quit as exit status $QUIT_STATUS"
    hold "$PLAYER does not report quit as exit status $QUIT_STATUS; rebuild it."
    exit 1
fi

run=0
while :; do
    run=$((run + 1))
    "$AFPLAY" "$AUDIO" &
    A=$!
    t0=$SECONDS
    "$PLAYER" "$ASSET" "$@" 2>"$ERR"
    rc=$?
    secs=$((SECONDS - t0))
    kill "$A" 2>/dev/null
    wait "$A" 2>/dev/null
    A=
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
