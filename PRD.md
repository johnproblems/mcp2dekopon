# PRD: mcp2dekopon — generate Dekopon providers from MCP servers

## Summary and verification

`mcp2dekopon` is a CLI that connects to an MCP server, reads its tool list, and writes a buildable `dekopon-provider-sdk` 0.46.1 crate plus a starter broker config, so any HTTP MCP server becomes a sandboxed, policy-gated Dekopon provider.

**Verified gap.** The [roadmap](https://raw.githubusercontent.com/dekopon-agents/dekopon/main/docs/roadmap.md) lists "MCP interoperability" as a next milestone and reserves `dekopon-mcp`, which is not published. The changelog never mentions MCP or OpenAPI, and none of the 30 `dekopon*` crates on crates.io does either. Building a generator out-of-tree does not compete with whatever core designs later; it produces ordinary providers.

## Goals, non-goals, success criteria

**Goals**

- `mcp2dekopon gen --url <mcp-endpoint> --id <provider-id> --out <dir>` produces a crate that compiles to `wasm32` with no hand edits for well-formed servers.
- One MCP tool becomes one capability; one command word per provider, one subcommand per tool.
- Every generated input schema passes the SDK's closed-schema check, or the tool is skipped with a reason in a report.
- A starter `broker.d/<provider-id>.yaml` fragment: per-capability `http` constraints pinned to the server's one origin, and a Cedar file granting nothing by default.

**Non-goals (v0.1)**

- stdio MCP servers (a WASM provider can only speak HTTP through the broker).
- MCP resources, prompts, sampling and notifications; tools only.
- Incremental SSE streaming to the model; responses are buffered.
- Guessing effect labels with confidence; the generator marks everything `ExternalWrite` / `Medium` unless the tool's MCP `annotations` say read-only, and tells the operator to review.

**Success criteria**

| Criterion | Target |
| --- | --- |
| Reference servers converted and building | 3 public HTTP MCP servers |
| Generated manifests passing `testkit::conformance()` | 100% |
| Tools skipped, each with a written reason | reported, never silent |
| End-to-end call through the testkit fake broker against a loopback MCP mock | passes |

## How it works

The generator runs three stages: fetch the tool list, tighten each schema until the SDK will accept it, then render Rust and YAML from templates.

1. **Fetch.** POST `initialize`, then `tools/list` (following pagination) over MCP Streamable HTTP; read `name`, `description`, `inputSchema`, and `annotations` (`readOnlyHint`, `destructiveHint`).
2. **Tighten.** The SDK refuses any `$ref` and any object without `additionalProperties: false`. Rules, applied in order:
   - inline local `$ref`/`$defs`; a recursive reference skips the tool (reason `recursive-schema`);
   - set `additionalProperties: false` on every object; an object with only `additionalProperties` and no `properties` (a free map) becomes a JSON string argument the generated code parses, flagged `free-map-as-string`;
   - `oneOf`/`anyOf` of objects becomes a Rust enum when each branch is closed, else the tool is skipped;
   - names become kebab-case flags and snake\_case Rust fields with `#[serde(rename)]` back to the MCP name.
3. **Render.** One `Capability` per tool (`NAME` = sanitized tool name, at most 32 per provider; a larger server is split into `<id>-1`, `<id>-2`); `Needs = (Http,)`; `run` does `initialize` then `tools/call` as buffered JSON-RPC POSTs to `Base` from `providerSettings.<id>.baseUrl`, and prints the MCP `content` as one JSON line.

**Generated broker fragment** (from the [brokerd contract](https://raw.githubusercontent.com/dekopon-agents/dekopon/main/crates/dekopon-brokerd/README.md)): `capabilities.<id>` with `http.allowedHosts: [<origin host>]`, `allowedMethods: [POST]`, `maxRequests: 2` (initialize plus call), byte limits, and a commented `credential:` slot. Auth uses the broker's credential or public-DRN path; the generated provider never sees a token.

**Effect labels.** `readOnlyHint: true` maps to `ReadOnly`, `destructiveHint: true` to `ExternalWrite` with `High` risk, and everything else to `ExternalWrite` with `Medium`. The report lists every label for operator review, because a wrong `ReadOnly` would place the capability in the provider's `read-only` Cedar group.

## Build plan, testing, risks

A working generator for one reference server lands by hour 12; hours 12–24 widen schema coverage and harden the output.

| Hours | Milestone |
| --- | --- |
| 0–3 | CLI skeleton; fetch `tools/list` from a loopback mock; snapshot tool JSON |
| 3–8 | Schema tightener with the rules above; golden tests per rule |
| 8–12 | Templates; first generated crate compiles to wasm32 and passes `conformance()` |
| 12–18 | Broker fragment, skip report, 32-tool splitting, two more real servers |
| 18–24 | Testkit end-to-end call against the mock; README with a full walkthrough |

**Testing.** Golden-file tests for each tightening rule; generated crates built in CI and run through `testkit::conformance()`; one end-to-end call per reference server through the testkit fake broker against a loopback MCP mock.

**Risks**

| Risk | Mitigation |
| --- | --- |
| Stateful MCP servers expect a session id across calls, but a capability can't hold HTTP and storage together | Re-run `initialize` inside every invocation (costs one extra request); list servers that reject this as unsupported |
| Real-world tool schemas are messier than the rules expect | Skip with a reason rather than generate something wrong; track the skip rate as a metric |
| Responses arrive as SSE | Read the buffered body and parse the final `result` event; no streaming to the model |
| SDK churn between releases | Pin `=0.46.1` in generated crates; record the SDK version in a generated header |

**Open questions**

- Should the generator also emit a Cedar policy that grants only the `read-only` group, as a safe default?
- Is OpenAPI a cheap second input format once the tightener exists?
