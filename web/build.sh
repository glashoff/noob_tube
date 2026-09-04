#!/usr/bin/env bash
#
# Builds the browser client into `web/dist`: cargo, then wasm-bindgen, then the page beside them.
#
#   ./web/build.sh              the `web` profile — what you serve, and the only thing worth loading
#   ./web/build.sh --debug      the dev profile, for a quick "does it boot" on this machine
#
# The `web` profile is the default and it is not a preference. A dev-profile wasm build of this tree
# is over two hundred megabytes, which a browser will compile eventually and nobody will wait for
# twice; see the profile in the workspace `Cargo.toml` for what it does about that and why plain
# `release` is not it either.
#
# What comes out is a directory of static files, plus one thing that is not static: `net-config`,
# which the page fetches from its own origin and which has to be proxied to the server's metadata
# port. `web/serve.py` does that for a machine you are sitting at; a deployment does it in the
# reverse proxy. See web.md §2 and §8.

set -euo pipefail

cd "$(dirname "$(readlink -f "$0")")/.."

profile=web
target_dir=web
for arg in "$@"; do
    case "$arg" in
        --debug) profile=dev; target_dir=debug ;;
        *) echo "$0: unknown argument $arg" >&2; exit 2 ;;
    esac
done

wasm=target/wasm32-unknown-unknown/$target_dir/noob_tube_client.wasm
out=web/dist

if ! command -v wasm-bindgen >/dev/null; then
    # The version has to match the `wasm-bindgen` crate in Cargo.lock exactly: the generated
    # JavaScript and the generated wasm agree on an ABI that is not stable between releases.
    version=$(sed -n '/^name = "wasm-bindgen"$/{n;s/version = "\(.*\)"/\1/p;}' Cargo.lock | head -1)
    echo "wasm-bindgen is not installed: cargo install wasm-bindgen-cli --version $version" >&2
    exit 1
fi

echo "building $profile for wasm32-unknown-unknown"
cargo build --profile "$profile" --target wasm32-unknown-unknown -p noob_tube_client

echo "generating the bindings"
wasm-bindgen --target web --no-typescript --out-dir "$out" "$wasm"

cp web/index.html "$out/"

# The assets, as a link rather than a copy. Three hundred megabytes duplicated into a build
# directory is three hundred megabytes to keep in step by hand, and what the browser fetches is
# whatever the link points at — which is what makes editing a shader and reloading the page work.
# A deployment copies instead; see web.md §8.
ln -sfn ../../assets "$out/assets"

printf 'built %s (%s)\n' "$out" "$(du -h "$out/noob_tube_client_bg.wasm" | cut -f1)"
