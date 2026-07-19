#!/bin/sh
# Minimal supervisor wrapper for the TETRA BlueStation MS stack, as an
# alternative to systemd (NON-STANDARD, Plane B management interface).
#
# The MS management interface applies a staged configuration by having the stack
# de-register gracefully and then exit with code 75 (EXIT_RESTART). This loop
# respawns the process on exit code 75 so it reloads the new configuration, and
# exits otherwise (e.g. 0 for a clean Ctrl+C shutdown, non-zero/non-75 for a
# genuine failure).
#
# Usage: bluestation-ms-supervisor.sh /path/to/config-ms.toml

set -eu

BIN="${BLUESTATION_BIN:-/usr/local/bin/bluestation-bs}"
CONFIG="${1:?usage: $0 <config.toml>}"
EXIT_RESTART=75

while true; do
    set +e
    "$BIN" "$CONFIG"
    code=$?
    set -e
    if [ "$code" -eq "$EXIT_RESTART" ]; then
        echo "bluestation-ms: config-apply restart requested (exit $code); respawning..." >&2
        continue
    fi
    echo "bluestation-ms: exited with code $code; not restarting." >&2
    exit "$code"
done
