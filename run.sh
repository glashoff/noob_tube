#!/usr/bin/env bash
# Start a binary from the workspace build without going through `cargo run`.
#
# Two reasons it is a script rather than `cargo run`. `cargo run` needs `-p`, which narrows the
# package selection and therefore rebuilds against a different feature unification — see
# `.cargo/config.toml`. And the dev profile links Bevy dynamically, so the binary needs to find
# `libbevy_dylib.so` and the toolchain's `libstd`, which `cargo run` would have set for us.
#
# Usage:  ./run.sh client [args...]   ./run.sh server [args...]
set -euo pipefail

cargo dev

target=target/debug
# `libstd` lives under rustlib, not in the sysroot's own lib/ — the dylib links it dynamically too,
# not just Bevy, and the binary will not start without both directories on the path.
rustlib="$(rustc --print sysroot)/lib/rustlib/$(rustc -vV | sed -n 's/^host: //p')/lib"
export LD_LIBRARY_PATH="$target/deps:$rustlib${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"

case "${1:-client}" in
    client) shift || true; exec "$target/noob_tube_client" "$@" ;;
    # The dedicated binary, on its own. `./run.sh client server` hosts instead — the same server on
    # a thread beside a client in one process, which is the one to use while both sides are being
    # changed together.
    server) shift || true; exec "$target/noob_tube_server" "$@" ;;
    *) echo "usage: $0 {client|server} [args...]" >&2; exit 2 ;;
esac
