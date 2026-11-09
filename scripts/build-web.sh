#!/usr/bin/env bash
# Builds the playground into web/, ready to serve: python3 -m http.server -d web
set -euo pipefail
cd "$(dirname "$0")/.."
# The playground favours a small download over the last bit of speed.
CARGO_PROFILE_RELEASE_OPT_LEVEL=s cargo build --lib --release --target wasm32-unknown-unknown
cp target/wasm32-unknown-unknown/release/spola.wasm web/spola.wasm
rm -rf web/examples && cp -R examples web/examples
echo "web/spola.wasm: $(wc -c < web/spola.wasm) bytes"
