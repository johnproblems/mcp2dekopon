# PRD coverage: what the PRD asked for, where it lives, how it is verified

Each section of [PRD.md](../PRD.md) is named below with the code that
implements it and the test or gate that verifies it. Two deliberate deviations
are recorded at the end.

## Summary and verification

The CLI connects to an MCP server, reads its tool list, and writes a buildable
`dekopon-provider-sdk` 0.46.1 crate plus a starter broker config
(`src/main.rs`, `src/fetch.rs`, `src/tighten.rs`, `src/codegen.rs`).
Verified end to end by `scripts/e2e.sh` (mock), `scripts/e2e-split.sh`
(38-tool split), and `scripts/reference.sh` (three public servers), each of
which builds the generated crate to wasm32 and runs
`testkit::conformance()`.

## Goals, non-goals, success criteria

- `gen --url --id --out` produces a crate that compiles to wasm32 with no hand
  edits: `scripts/reference.sh` runs `./build.sh` on all three reference
  servers. Non-goals (stdio servers, resources/prompts, streaming,
  confident effect inference) hold: only `tools/list` and `tools/call` over
  Streamable HTTP are implemented, SSE answers are buffered
  (`e2e_error_search_sse` in generated tests covers SSE parsing), and effect
  labels default to `external-write`/`Medium`.
- One tool becomes one capability, one command word per provider, one
  subcommand per tool: `src/codegen.rs`; per-tool `propose_*` tests in every
  generated crate assert routing plus the assembled input.
- Every input schema passes the SDK closed-schema check, else the tool is
  skipped with a reason: `src/tighten.rs`; golden tests in `tests/tighten.rs`
  per rule, and `report.json` carries every skip
  (`gen_from_mock_writes_a_provider_and_a_report`).
- Starter `broker.d/<id>.yaml` plus a Cedar file granting nothing:
  `src/codegen.rs` broker fragment; golden `gen_mock_broker` snapshot.
- Success criteria: 3 reference servers (`scripts/reference.sh`:
  `REFERENCE OK`), 100% `conformance()` (every generated crate runs
  `tests/conformance.rs`), skips reported never silent (`summary` block in
  `report.json`), loopback e2e call (`scripts/e2e.sh`: `E2E OK`).

## How it works

- Fetch (`src/fetch.rs`): `initialize`, then paginated `tools/list`, reading
  `name`, `description`, `inputSchema`, `annotations`. Retry covers timeouts,
  refused connections, 5xx, and 429 (honoring `Retry-After` up to 30 s);
  401/403/407 stop with a `--header` hint; an expired session (400/404/409)
  re-initializes once per page; a repeated cursor fails fast; bodies and
  captures over 16 MiB are refused. Every request carries a
  `mcp2dekopon/<version>` user-agent. Verified by the `Mode` matrix in
  `tests/gen_mock.rs` (flaky, infinite-cursor, repeated-cursor, wrong-id,
  401, 404, 409, rate-limited, persistent-429, oversized-body, empty-name,
  old-version, many-tools).
- Tighten (`src/tighten.rs`): local `$ref`/`$defs` inlined (JSON pointers
  included), recursion skipped (`recursive-schema`), `additionalProperties:
  false` everywhere, free maps as parsed JSON strings (`free-map-as-string`),
  `oneOf`/`anyOf` of objects as untagged enums, kebab-case flags (camel
  humps split) and `snake_case` fields with `#[serde(rename)]`. Widenings
  beyond the PRD: scalar unions, `allOf` merging, nested `oneOf`/`anyOf`/
  `allOf` flattening, identical-scalar collapse, nullable union branches,
  boolean schemas, percent-decoded `$ref`s, a depth bound
  (`schema-too-deep`), and warnings for dropped logic keywords
  (`schema-constraint-dropped`, `tuple-validation-dropped`). Verified by the
  37 golden tests in `tests/tighten.rs`.
- Render (`src/codegen.rs`): one `Capability` per tool, at most 32 per
  provider, larger servers split into `<id>-1`, `<id>-2`
  (`split_tools` unit tests, with split-aware name budgets); `run` replays
  `initialize`, the `notifications/initialized` notice, then `tools/call`,
  verifies the call response id, and prints MCP `content` as one JSON line;
  `isError` answers print then fail. Verified by generated `e2e_*` tests and
  the error-path tests for the first tool of every provider.
- Broker fragment: per-capability `http` grants pinned to one origin,
  POST-only, `maxRequests: 3`, byte limits, a `timeoutMs` mirroring
  `--timeout-secs`, commented `credential:` slot. The provider never sees a
  token: fetch `--header` values are never written to generated files (only
  used for `tools/list`).
- Effect labels: `readOnlyHint` to `read-only`/`Low`, `destructiveHint` to
  `external-write`/`High`, else `external-write`/`Medium`; every label lands
  in `report.json` for operator review (asserted per capability in
  `gen_from_mock_writes_a_provider_and_a_report`).

## Build plan, testing, risks

Milestones 0–24 map to `scripts/e2e.sh`, `tests/tighten.rs` goldens,
`scripts/reference.sh`, the broker fragment plus `report.json` plus splitting,
and this README walkthrough, respectively. Testing exceeds the plan: golden
tests per rule, generated crates built in CI with `conformance()`, and one
e2e call per reference server, plus generated error-path, propose, CLI-help,
manifest, and unknown-capability tests.

- Stateful servers: every invocation re-runs `initialize` plus the
  `notifications/initialized` notice (two extra requests); no allowlist of
  rejecting servers has been needed so far, and adding one is a data change,
  not a code change.
- Messy schemas: skipped with a reason; the skip rate is auditable via the
  report `summary` block.
- SSE answers: buffered and parsed (`post_once` in `src/fetch.rs`,
  `read_mcp_response` in generated code).
- SDK churn: generated crates pin `=0.46.1` and the report records
  `sdk_version`; the generator vendors the SDK 0.46.1 WIT worlds.
- Open questions (Cedar read-only default, OpenAPI input) remain open, as in
  the PRD.

## Deliberate deviations from the PRD

1. `Needs = (Settings, Http)` instead of `(Http,)`. The PRD's own Render rule
   requires reading `Base` from `providerSettings.<id>.baseUrl`, which needs
   the `Settings` need; the implemented shape honors that requirement and
   enables pointing a deployment at another endpoint without rebuilding.
2. The report carries `format_version`, `elapsed_ms`, `options`, `summary`,
   `warnings`, and per-skip `description` fields the PRD does not name.
   These are additive: every PRD-required element (capability labels, skip
   reasons) is present unchanged.
3. Every call sends the `notifications/initialized` notice (best effort)
   between `initialize` and `tools/call`, and the broker fragment allows
   `maxRequests: 3` instead of the PRD's 2. Strict servers reject calls
   without the notice; lenient ones ignore it, so the notice maximizes
   interop at the cost of one request the PRD did not budget.
