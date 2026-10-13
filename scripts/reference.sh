#!/usr/bin/env bash
# Reference validation: convert three public HTTP MCP servers, build each
# generated provider natively and to wasm, and run its tests + conformance.
# Usage: scripts/reference.sh [workdir]
set -euo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)
work=${1:-"$root/out-reference"}
rm -rf "$work"
mkdir -p "$work"

cargo build --manifest-path "$root/Cargo.toml" --locked
gen="$root/target/debug/mcp2dekopon"

convert() {
  local name=$1 url=$2 id=$3
  echo "== $name =="
  "$gen" gen --url "$url" --id "$id" --out "$work/$name"
  local dirs
  dirs=$(python3 -c "
import json
report = json.load(open('$work/$name/report.json'))
providers = report['providers']
assert providers, 'no providers generated'
print(' '.join(p['dir'] for p in providers))
" | tail -1)
  python3 -c "
import json
report = json.load(open('$work/$name/report.json'))
caps = sum(len(p['capabilities']) for p in report['providers'])
print('capabilities:', caps, 'skipped:', [(s['tool'], s['reason']) for s in report['skipped']])
"
  for dir in $dirs; do
    echo "-- hygiene: $dir --"
    cargo fmt --manifest-path "$work/$name/$dir/Cargo.toml" --check
    cargo clippy --manifest-path "$work/$name/$dir/Cargo.toml" --all-targets -- -D warnings
    echo "-- native tests: $dir --"
    cargo test --manifest-path "$work/$name/$dir/Cargo.toml" 2>&1 | grep -E "test result|error" | head -5
    echo "-- wasm build + conformance: $dir --"
    (cd "$work/$name/$dir" && ./build.sh)
    cargo test --manifest-path "$work/$name/$dir/Cargo.toml" --test conformance 2>&1 | grep -E "test result"
  done
}

convert deepwiki https://mcp.deepwiki.com/mcp deepwiki
convert context7 https://mcp.context7.com/mcp context7
convert msdocs https://learn.microsoft.com/api/mcp msdocs

echo "REFERENCE OK"
