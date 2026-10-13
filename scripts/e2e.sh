#!/usr/bin/env bash
# Full end-to-end validation: mock MCP server -> generate -> native tests ->
# wasm component build -> conformance. Usage: scripts/e2e.sh [workdir]
set -euo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)
work=${1:-"$root/out"}
rm -rf "$work"
mkdir -p "$work"

echo "== build mcp2dekopon =="
cargo build --manifest-path "$root/Cargo.toml" --locked
gen="$root/target/debug/mcp2dekopon"

echo "== start loopback MCP mock =="
port_file=$(mktemp)
python3 "$root/scripts/mock-mcp.py" 0 >"$port_file" &
mock_pid=$!
trap 'kill $mock_pid 2>/dev/null || true; rm -f $port_file' EXIT
for _ in $(seq 1 50); do
  [ -s "$port_file" ] && break
  sleep 0.1
done
port=$(cat "$port_file")
url="http://127.0.0.1:$port/mcp"
echo "mock at $url"

echo "== generate =="
"$gen" gen --url "$url" --id mockmcp --out "$work"
cat "$work/report.json"

provider="$work/mockmcp"
echo "== generated code hygiene =="
cargo fmt --manifest-path "$provider/Cargo.toml" --check
cargo clippy --manifest-path "$provider/Cargo.toml" --all-targets -- -D warnings
echo "== native tests (fake broker e2e) =="
cargo test --manifest-path "$provider/Cargo.toml" -- --nocapture

echo "== wasm component build =="
(cd "$provider" && ./build.sh)

echo "== conformance (real component) =="
cargo test --manifest-path "$provider/Cargo.toml" --test conformance -- --nocapture

echo "== auth headers reach the server =="
auth_file=$(mktemp)
python3 "$root/scripts/mock-mcp.py" --require-auth "Bearer test-token" 0 >"$auth_file" &
auth_pid=$!
for _ in $(seq 1 50); do
  [ -s "$auth_file" ] && break
  sleep 0.1
done
auth_url="http://127.0.0.1:$(cat "$auth_file")/mcp"
if "$gen" gen --url "$auth_url" --id authmock --out "$work-auth" 2>/dev/null; then
  echo "error: unauthenticated fetch should have failed" >&2
  exit 1
fi
"$gen" gen --url "$auth_url" --id authmock --out "$work-auth" --header "Authorization: Bearer test-token" >/dev/null
[ -f "$work-auth/report.json" ] || { echo "error: authenticated fetch produced no report" >&2; exit 1; }
kill $auth_pid 2>/dev/null || true
rm -f "$auth_file"
rm -rf "$work-auth"
echo "auth ok"

echo "E2E OK: $provider"
