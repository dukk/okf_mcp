// Drops one harmless warning: ajv's `unknown format "uint32" ignored in schema`.
//
// okf-mcp's tool schemas come from schemars, which tags numbers with their Rust
// width (`format: "uint"`, `"uint32"`, `"uint64"`, `"int64"`, `"double"`, ...),
// formats ajv doesn't know. Which ones depends on the server's schemars
// version. The MCPJam SDK (10.11.0) builds its validator
// (`DialectAwareJsonSchemaValidator`) without ajv's `logger: false` and gives
// no way to swap it out. So every MCP connection prints that notice once per
// such field, which buries the test report.
// Validation is unaffected: ajv just skips the unknown format.
//
// Imported by the global setup (main process) and as a vitest setup file
// (every worker); importing it twice is harmless.

// Only schemars' numeric formats: an unknown format of any other name still
// prints, in case it is a real mistake in a schema.
const NUMERIC_FORMAT_NOTICE = /^unknown format "(?:u?int\d*|float|double)" ignored in schema/;
const QUIETED = Symbol.for("okf-mcp.mcpjam.quiet-ajv");

// `globalThis` with a symbol key marks the patch, so a second import is a no-op.
const marked = globalThis as typeof globalThis & { [QUIETED]?: true };
if (marked[QUIETED] === undefined) {
    marked[QUIETED] = true;
    const warn = console.warn.bind(console);
    console.warn = (...args: unknown[]): void => {
        if (typeof args[0] === "string" && NUMERIC_FORMAT_NOTICE.test(args[0])) return;
        warn(...args);
    };
}
