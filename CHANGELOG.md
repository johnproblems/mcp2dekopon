# Changelog

## 0.2.0

Fifty improvements across fetching, tightening, naming, the CLI, the report,
the generators, and the docs. Every behavior change is covered by the test
suite (`cargo test`).

### Fetching

- Retry rate-limited answers (429) honoring `Retry-After` (clamped to 30 s);
  exhausted rate limits report `rate limiting` instead of a bare status.
- Re-initialize once on 409 session conflicts, as well as 400/404.
- Fail fast on repeated `nextCursor` pagination loops, naming the cursor.
- Cap ingested bodies at 16 MiB, for fetch responses and `--from-file`
  captures alike.
- Send `User-Agent: mcp2dekopon/<version>` on every fetch request.
- Include the received id in response-id mismatch errors, and truncated
  server `error.data` in RPC errors.
- Reject empty tool names at parse time with a clear error.
- Hint at the `--url` path on 404 during `initialize`.
- Strip UTF-8 BOMs before parsing RPC bodies; accept lone-CR SSE line endings.
- Cover IPv6 origins (`[::1]` loopback included) with regression tests.

### Tightening

- Flatten bare nested `oneOf`/`anyOf` (including through `$ref`) and nested
  `allOf` in place, instead of skipping; cyclic nests skip as
  `recursive-schema`.
- Collapse unions whose branches lower to one identical scalar to that
  scalar, instead of skipping.
- Accept nullable (`["object", "null"]`) branches in object unions and
  nullable members in `allOf` merges.
- Lower boolean schemas (`true` to `Any`, `false` to `Any` with a
  `false-schema-loosened` warning) and empty `type: []` (to `Any` with an
  `empty-type-loosened` warning) instead of skipping.
- Percent-decode local `$ref` fragments before pointer resolution.
- Record `schema-constraint-dropped` when `not`, `if`/`then`/`else`,
  `dependentSchemas`, `contains`, or `propertyNames` are dropped, and
  `tuple-validation-dropped` for `prefixItems`; a top-level `not` no longer
  skips the tool.
- Record `enum-not-constrained` for mixed-kind and empty `enum` sets.

### Naming

- Split camel humps when sanitizing tool names (`readWikiContents` to
  `read-wiki-contents`).
- Reserve keyword-mapped field spellings while deduping (`self_`/`r#type`
  cannot be reused by a later field).
- Never emit the reserved `Self` as a generated type name.
- Budget capability names against the longest split provider id, so
  `provider.name` fits even after a `<id>-N` split (no length underflow on
  maximal ids).

### CLI

- New `--retries N` (default 3) and `--max-tools N` (default 1000) flags.
- `--save-tools` and `--dry-run` conflict instead of silently preferring the
  save.
- No protocol-version warning under `--from-file` (nothing is negotiated).
- Suggest the closest valid `--id` when validation fails.
- Reject an empty `--protocol-version`.
- Validate `--header` strictly (RFC token names, no control characters,
  protocol headers reserved) and trim values on both ends.
- Print the plaintext-outside-loopback warning to stderr, not just into the
  broker fragment.

### Report

- New `format_version: 1`, `elapsed_ms`, and `options` (offered protocol
  version, timeout, retries, tool cap) fields.
- `dir` is the out-joined provider path, as the README always showed.
- Truncate over-long skipped-tool descriptions to 1000 characters.

### Generated providers

- Send the `notifications/initialized` notice (best effort, session-aware)
  before `tools/call`; broker fragments allow `maxRequests: 3`, and the
  generated tests assert all three legs.
- Verify the `tools/call` JSON-RPC response id.
- `timeoutMs` in the broker fragment mirrors `--timeout-secs`.
- `McpError::Protocol` carries an owned `String`; malformed-body errors quote
  a 120-character snippet of the offending body.
- Escape pipes and newlines in generated README tables, and add a security
  note for plaintext origins.
- `build.sh` accepts any `wasm-tools 1.259.x` without warning.
- Provider directories are staged then swapped into place (`--force` replaces
  the directory wholesale, so stale artifacts cannot survive); `report.json`
  and `--save-tools` captures write atomically.

### Docs and scripts

- README documents every new flag, warning code, and behavior; the
  `report.json` example matches the writer.
- `docs/PRD-coverage.md` records the new behaviors and deviations.
- `scripts/mock-mcp.py` validates `--extra` (non-negative) and the port
  number (0–65535).

## 0.1.0

Initial release: fetch `tools/list` over MCP Streamable HTTP, tighten schemas
until the provider SDK accepts them, and render buildable
`dekopon-provider-sdk` 0.46.1 crates with broker fragments, Cedar starters,
per-provider READMEs, and a `report.json` of labels and skips.
