# mcpjam scenarios: silence the SDK's ajv format notices

## What

`tests/mcpjam/src/quiet-ajv.ts` drops ajv's `unknown format "<f>" ignored in
schema` warning for schemars' numeric formats (`uint`, `uint32`, `uint64`,
`int*`, `float`, `double`). An unknown format of any other name still prints.
It runs in the global setup (main process) and as a vitest `setupFiles` entry
(every worker). It replaces the `onConsoleLog` filter, which covered only the
workers and only plain `uint`.

## Why

The MCPJam SDK (10.11.0) builds its `DialectAwareJsonSchemaValidator` without
ajv's `logger: false`, and `MCPClientManager` gives no way to replace it. So
every MCP connection printed one notice per numeric field in okf-mcp's tool
schemas. A local run printed 512 of them (480 `uint32`, 32 `uint64`), and a
staging run, whose build emits plain `uint`, buried the test report.
Validation is unaffected: ajv skips the unknown format.

## Verification

A local run prints only the target line and the vitest summary: 27 passed
plus 1 expected fail, with no format notices.
