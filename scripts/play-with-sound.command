#!/usr/bin/env bash
# Loop `auto-ascii play` on one asset until the viewer quits (exit status 3).
# Usage: play-with-sound.command CLI ASSET [PLAY_ARGS...], where CLI is the
# auto-ascii binary. Every exit goes to $LOG, and an unexpected one keeps the
# window open until Return is pressed.
set -u

if [ $# -lt 2 ]; then
    echo "usage: $0 CLI ASSET [PLAY_ARGS...]" >&2
    exit 2
fi
CLI=$1 ASSET=$2
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

if ! "$CLI" play --help 2>/dev/null | grep -q '3 when the viewer quits'; then
    log "refusing $CLI: its play does not report quit as exit status $QUIT_STATUS"
    hold "$CLI play does not report quit as exit status $QUIT_STATUS; rebuild it."
    exit 1
fi

run=0
while :; do
    run=$((run + 1))
    t0=$SECONDS
    "$CLI" play "$ASSET" "$@" 2>"$ERR"
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
            hold "auto-ascii play exited with status $rc after ${secs}s (run $run)."
            exit "$rc"
            ;;
    esac
done
