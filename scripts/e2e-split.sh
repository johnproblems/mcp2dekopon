#!/usr/bin/env bash
# Split validation: a 38-tool mock server (37 usable, 1 skipped) becomes two
# providers (<id>-1, <id>-2), each built and tested. Usage: scripts/e2e-split.sh [workdir]
set -euo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)
work=${1:-"$root/out-split"}
rm -rf "$work"
mkdir -p "$work"

cargo build --manifest-path "$root/Cargo.toml" --locked
gen="$root/target/debug/mcp2dekopon"

echo "== start 38-tool mock =="
port_file=$(mktemp)
python3 "$root/scripts/mock-mcp.py" --extra 32 0 >"$port_file" &
mock_pid=$!
trap 'kill $mock_pid 2>/dev/null || true; rm -f $port_file' EXIT
for _ in $(seq 1 50); do
  [ -s "$port_file" ] && break
  sleep 0.1
done
url="http://127.0.0.1:$(cat "$port_file")/mcp"

echo "== generate =="
"$gen" gen --url "$url" --id bigmock --out "$work"
python3 -c "
import json
report = json.load(open('$work/report.json'))
ids = [p['id'] for p in report['providers']]
assert ids == ['bigmock-1', 'bigmock-2'], ids
counts = [len(p['capabilities']) for p in report['providers']]
assert counts == [32, 5], counts
print('split ok:', ids, counts)
"

for part in bigmock-1 bigmock-2; do
  echo "== native tests: $part =="
  cargo test --manifest-path "$work/$part/Cargo.toml" -- --nocapture 2>&1 | grep -E "test result|error" | head -5
done

echo "== wasm build + conformance: bigmock-1 =="
(cd "$work/bigmock-1" && ./build.sh)
cargo test --manifest-path "$work/bigmock-1/Cargo.toml" --test conformance 2>&1 | grep -E "test result"

echo "== wasm build + conformance: bigmock-2 =="
(cd "$work/bigmock-2" && ./build.sh)
cargo test --manifest-path "$work/bigmock-2/Cargo.toml" --test conformance 2>&1 | grep -E "test result"

echo "SPLIT E2E OK"
