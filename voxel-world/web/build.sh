#!/usr/bin/env bash
# Build the generator as WebAssembly and copy it next to the viewer: web/www/gen.wasm.
#   ./build.sh              release wasm only
#   ./build.sh --bookmarks  also regenerate web/www/bookmarks.json (native scout, ~1 min)
#   ./build.sh --test       also run the native unit tests of the wasm crate
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
OFFLINE="${CARGO_OFFLINE:---offline}"
mkdir -p "$HERE/www"

cd "$HERE/wasm"
if [[ " $* " == *" --test "* ]]; then
  cargo test --release $OFFLINE -- --test-threads 2
fi
# no wasm-bindgen / wasm-opt: plain cdylib, size-tuned release profile (see wasm/Cargo.toml)
cargo build --release --target wasm32-unknown-unknown $OFFLINE
cp target/wasm32-unknown-unknown/release/vw_web.wasm "$HERE/www/gen.wasm"
SIZE=$(stat -c %s "$HERE/www/gen.wasm" 2>/dev/null || stat -f %z "$HERE/www/gen.wasm")
GZ=$(gzip -9 -c "$HERE/www/gen.wasm" | wc -c)
echo "gen.wasm: $SIZE bytes ($((SIZE / 1024)) KiB), gzip -9: $GZ bytes ($((GZ / 1024)) KiB)"

if [[ " $* " == *" --bookmarks "* ]]; then
  cd "$HERE/tools"
  cargo run --release $OFFLINE --bin bookmarks -- "$HERE/www/bookmarks.json"
fi
