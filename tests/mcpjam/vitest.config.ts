import { defineConfig } from "vitest/config";

export default defineConfig({
    test: {
        include: ["scenarios/**/*.test.ts"],
        globalSetup: ["src/global-setup.ts"],
        // A remote target (staging) answers slower than a local server, and the
        // concurrency scenarios fan out many calls.
        testTimeout: 60_000,
        hookTimeout: 120_000,
        // Each file is one persona; files run in parallel, their tests in order.
        // Every scenario writes under its own folder, so parallel files never
        // collide, except in concurrency.test.ts, which collides on purpose.
        fileParallelism: true,
        // The SDK's schema validator warns about schemars' `format: "uint"`
        // once per tool field; it changes nothing, so keep it out of the report.
        onConsoleLog: (log) => !log.startsWith('unknown format "uint"'),
    },
});
