// Picks the target, seeds every scenario's folder, provides the target to the
// test files, and cleans up afterwards.
//
// Remote (OKF_MCP_URL set, e.g. staging): uses OKF_MCP_KEY and OKF_MCP_CATALOG
// as given. Nothing is created outside this run's folder.
//
// Local (no OKF_MCP_URL): needs DATABASE_URL or TEST_PG_URL (`just
// test-mcpjam` provides a throwaway Postgres), creates a user, a catalog and a
// write key with the okf-mcp CLI, and serves them on a free loopback port.
import { execFile, spawn, type ChildProcess } from "node:child_process";
import { randomBytes } from "node:crypto";
import { createServer } from "node:net";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { promisify } from "node:util";

import type { TestProject } from "vitest/node";

import { OkfSession, type Target } from "./okf.js";
import { SCENARIOS, folderOf } from "./scenarios.js";

declare module "vitest" {
    export interface ProvidedContext {
        target: Target;
    }
}

const run = promisify(execFile);
const REPO_ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "../../..");

export default async function setup(project: TestProject): Promise<() => Promise<void>> {
    // Lowercase kebab-case, as concept paths want: `swarm-<time>-<random>`.
    const runFolder = `swarm-${Date.now().toString(36)}-${randomBytes(3).toString("hex")}`;
    const local = process.env.OKF_MCP_URL === undefined ? await startLocal(runFolder) : null;
    const target = local?.target ?? remoteTarget(runFolder);

    await seed(target);
    project.provide("target", target);
    console.info(`mcpjam scenarios: ${target.mode} ${target.url}, ${target.catalog}/${runFolder}`);

    return async () => {
        if (target.mode === "remote" && process.env.OKF_MCP_KEEP !== "1") {
            await cleanUp(target);
        }
        local?.server.kill("SIGTERM");
    };
}

function remoteTarget(runFolder: string): Target {
    const required = (name: string): string => {
        const value = process.env[name];
        if (value === undefined || value === "") {
            throw new Error(`OKF_MCP_URL is set, so ${name} is required too`);
        }
        return value;
    };
    return {
        mode: "remote",
        url: required("OKF_MCP_URL"),
        key: required("OKF_MCP_KEY"),
        catalog: required("OKF_MCP_CATALOG"),
        runFolder,
    };
}

interface Local {
    target: Target;
    server: ChildProcess;
}

async function startLocal(runFolder: string): Promise<Local> {
    const databaseUrl = process.env.DATABASE_URL ?? process.env.TEST_PG_URL;
    if (databaseUrl === undefined) {
        throw new Error(
            "set OKF_MCP_URL (+ OKF_MCP_KEY, OKF_MCP_CATALOG) for a remote target, " +
                "or DATABASE_URL / TEST_PG_URL for a local one (`just test-mcpjam` starts one)",
        );
    }
    const bin = process.env.OKF_MCP_BIN ?? resolve(REPO_ROOT, "target/debug/okf-mcp");
    const env = { ...process.env, DATABASE_URL: databaseUrl };
    const cli = async (...args: string[]): Promise<string> =>
        (await run(bin, args, { env })).stdout.trim();

    // Names unique to this run, so a reused Postgres (TEST_PG_URL) is fine.
    const name = runFolder;
    await cli("migrate");
    await cli("user", "create", name);
    await cli("catalog", "create", name, "--owner", name);
    const key = await cli(
        "key", "create", name,
        "--access", "write", "--catalogs", name, "--default", name, "--user", name,
    );

    const port = await freePort();
    const server = spawn(bin, ["serve", "--listen", `127.0.0.1:${port}`], {
        // Warnings and errors only, unless the caller asks for more.
        env: { ...env, RUST_LOG: process.env.RUST_LOG ?? "warn" },
        stdio: ["ignore", "ignore", "inherit"],
    });
    const base = `http://127.0.0.1:${port}`;
    await waitHealthy(`${base}/healthz`, server);
    return {
        target: { mode: "local", url: `${base}/mcp`, key, catalog: name, runFolder },
        server,
    };
}

async function freePort(): Promise<number> {
    return new Promise((resolvePort, reject) => {
        const probe = createServer();
        probe.once("error", reject);
        probe.listen(0, "127.0.0.1", () => {
            const address = probe.address();
            const port = typeof address === "object" && address !== null ? address.port : 0;
            probe.close(() => resolvePort(port));
        });
    });
}

async function waitHealthy(url: string, server: ChildProcess): Promise<void> {
    const deadline = Date.now() + 60_000;
    while (Date.now() < deadline) {
        if (server.exitCode !== null) {
            throw new Error(`okf-mcp serve exited with ${server.exitCode}`);
        }
        try {
            if ((await fetch(url)).ok) return;
        } catch {
            // Not listening yet.
        }
        await new Promise((wait) => setTimeout(wait, 250));
    }
    throw new Error(`okf-mcp serve was not healthy at ${url} after 60s`);
}

// One changeset, one publish: every scenario's folder and seed concepts.
async function seed(target: Target): Promise<void> {
    const session = await OkfSession.open(target);
    try {
        const changeset = await session.openChangeset();
        for (const scenario of Object.values(SCENARIOS)) {
            for (const seed of scenario.seeds) {
                await session.call("create_concept", {
                    path: `${folderOf(target.runFolder, scenario)}/${seed.path}`,
                    frontmatter: {
                        type: seed.type,
                        title: seed.title,
                        description: seed.description,
                    },
                    body: seed.body,
                    index_summary: seed.description,
                    changeset,
                });
            }
        }
        await session.publish(changeset, `mcpjam scenarios: seed ${target.runFolder}`);
    } finally {
        await session.close();
    }
}

// Best effort: deletes the run's concepts. index.md files are never deleted
// (the server keeps them), so an empty folder index per scenario remains.
async function cleanUp(target: Target): Promise<void> {
    const session = await OkfSession.open(target);
    try {
        const paths = await session.searchUnder(`/${target.runFolder}`, "");
        const changeset = await session.openChangeset();
        const kept: string[] = [];
        for (const path of paths) {
            const outcome = await session.attempt("delete_concept", { path, changeset });
            if (!outcome.ok) kept.push(`${path} (${outcome.error.error})`);
        }
        await session.publish(changeset, `mcpjam scenarios: clean up ${target.runFolder}`);
        if (kept.length > 0) console.warn(`mcpjam scenarios: kept ${kept.join(", ")}`);
    } catch (error) {
        console.warn(`mcpjam scenarios: clean-up of ${target.runFolder} failed: ${error}`);
    } finally {
        await session.close();
    }
}
