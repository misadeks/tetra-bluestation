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
# Real-time scheduling & CPU pinning (IMPORTANT on a shared host)
# ---------------------------------------------------------------
# The MS is receive-timed: if the SDR RX pipeline is starved of CPU (e.g. a UI or
# desktop/display compositor running on the same board), samples are dropped
# ("Lost N samples, skipping ..."), the demodulator loses sync, and the MLE
# declares a serving-cell downlink failure (MLE-BREAK) — the radio flaps in and
# out of service and never registers. To prevent that, this wrapper launches the
# stack under the real-time FIFO scheduler (via `chrt`) and, if configured, pins
# it to dedicated CPUs (via `taskset`). Both are best-effort: if the tool is
# missing or the privilege is denied, it falls back to a normal launch.
#
# Environment knobs (all optional):
#   BLUESTATION_BIN   path to bluestation-bs (default /usr/local/bin/bluestation-bs)
#   RT_PRIO           real-time FIFO priority, 1..99 (default 73; empty disables)
#   CPU_AFFINITY      CPU list for taskset, e.g. "2,3" (default empty = no pin)
#
# Usage: bluestation-ms-supervisor.sh /path/to/config-ms.toml

set -eu

BIN="${BLUESTATION_BIN:-/usr/local/bin/bluestation-bs}"
CONFIG="${1:?usage: $0 <config.toml>}"
EXIT_RESTART=75
RT_PRIO="${RT_PRIO:-73}"
CPU_AFFINITY="${CPU_AFFINITY:-}"

# Default log verbosity: quiet `info` for steady-state operation. Override on
# demand for troubleshooting, e.g. `RUST_LOG=debug` or `RUST_LOG=trace`, or
# target one module: `RUST_LOG=tetra_entities::mm=debug`.
export RUST_LOG="${RUST_LOG:-info}"

# Build the launch prefix from whatever scheduling tools are available. Each is
# optional and degrades gracefully so the supervisor still works on a minimal
# system or without the privilege to set RT priority / affinity.
PREFIX=""
if [ -n "$RT_PRIO" ] && command -v chrt >/dev/null 2>&1; then
    PREFIX="chrt -f $RT_PRIO"
fi
if [ -n "$CPU_AFFINITY" ] && command -v taskset >/dev/null 2>&1; then
    PREFIX="taskset -c $CPU_AFFINITY $PREFIX"
fi

while true; do
    set +e
    # shellcheck disable=SC2086 # PREFIX is an intentional word-split command prefix.
    $PREFIX "$BIN" "$CONFIG"
    code=$?
    set -e
    if [ "$code" -eq "$EXIT_RESTART" ]; then
        echo "bluestation-ms: config-apply restart requested (exit $code); respawning..." >&2
        continue
    fi
    echo "bluestation-ms: exited with code $code; not restarting." >&2
    exit "$code"
done
