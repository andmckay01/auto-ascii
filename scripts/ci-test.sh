#!/usr/bin/env bash
set -euo pipefail

# Run the real-time playback and PTY tests with an application resource policy;
# taskpolicy's application type is inherited by cargo's test subprocesses.
if [[ $(uname -s) == Darwin ]]; then
    exec /usr/sbin/taskpolicy -a cargo test "$@"
fi
exec cargo test "$@"
