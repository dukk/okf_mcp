import { defineConfig } from "vitest/config";

export default defineConfig({
    test: {
        include: ["scenarios/**/*.test.ts"],
        globalSetup: ["src/global-setup.ts"],
        // Silences the SDK's ajv numeric-format notices in every worker (see the file).
        setupFiles: ["src/quiet-ajv.ts"],
        // A remote target (staging) answers slower than a local server, and the
        // concurrency scenarios fan out many calls.
        testTimeout: 60_000,
        hookTimeout: 120_000,
        // Each file is one persona; files run in parallel, their tests in order.
        // Every scenario writes under its own folder, so parallel files never
        // collide, except in concurrency.test.ts, which collides on purpose.
        fileParallelism: true,
    },
});
