// Picks the target and the isolation, creates and seeds the catalogs, provides
// the target to the test files, and cleans up afterwards.
//
// Target:
// - remote (OKF_MCP_URL set, e.g. staging): uses OKF_MCP_KEY as given;
// - local (no OKF_MCP_URL): needs DATABASE_URL or TEST_PG_URL (`just
//   test-mcpjam` provides a throwaway Postgres), creates a user and an admin
//   key reaching every catalog with the okf-mcp CLI, and serves them on a
//   free loopback port.
//
// Isolation (OKF_MCP_ISOLATION, default `auto`):
// - `catalog`: every scenario gets a temporary catalog of its own, made with
//   the REST admin API and deleted when the run ends. Needs a key that can
//   create catalogs and write in them: an admin key with `--all`.
// - `folder`: every scenario shares OKF_MCP_CATALOG, each in its own folder;
//   the run deletes its concepts afterwards. Works with a plain write key.
// - `auto`: `catalog` when the key may create catalogs, else `folder`.
import { execFile, spawn, type ChildProcess } from "node:child_process";
import { randomBytes } from "node:crypto";
import { createServer } from "node:net";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { promisify } from "node:util";

import type { TestProject } from "vitest/node";

import { CatalogAdmin } from "./admin.js";
import { OkfSession, type Target } from "./okf.js";
import { SCENARIOS, folderOf, type Scenario } from "./scenarios.js";

declare module "vitest" {
    export interface ProvidedContext {
        target: Target;
    }
}

const run = promisify(execFile);
const REPO_ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "../../..");

/** How to reach the server, before the catalogs are chosen. */
interface Connection {
    mode: Target["mode"];
    url: string;
    apiBase: string;
    key: string;
}

export default async function setup(project: TestProject): Promise<() => Promise<void>> {
    // Lowercase kebab-case, as catalog names and concept paths want.
    const runId = `${Date.now().toString(36)}-${randomBytes(3).toString("hex")}`;
    const runFolder = `swarm-${runId}`;
    const local = process.env.OKF_MCP_URL === undefined ? await startLocal(runId) : null;
    const connection = local?.connection ?? remoteConnection();
    const admin = new CatalogAdmin(connection.apiBase, connection.key, `mcpjam-${runId}`);
    const stopLocal = (): void => {
        local?.server.kill("SIGTERM");
    };

    let target: Target;
    try {
        const { isolation, catalogs } = await chooseCatalogs(admin, connection);
        target = {
            mode: connection.mode,
            url: connection.url,
            key: connection.key,
            isolation,
            catalogs,
            runFolder,
        };
        await seed(target);
    } catch (error) {
        // Vitest runs no teardown for a setup that threw: clean up here.
        await admin.deleteCreated();
        stopLocal();
        throw error;
    }

    project.provide("target", target);
    const where = target.isolation === "catalog"
        ? `${Object.keys(target.catalogs).length} temporary catalogs ${admin.prefix}-*`
        : `catalog ${Object.values(target.catalogs)[0]}, folder /${runFolder}`;
    console.info(`mcpjam scenarios: ${target.mode} ${target.url}, ${where}`);

    return async () => {
        const keep = process.env.OKF_MCP_KEEP === "1";
        if (keep) {
            console.info(`mcpjam scenarios: OKF_MCP_KEEP=1, kept ${where}`);
        } else if (target.isolation === "folder" && target.mode === "remote") {
            await cleanUpFolder(target);
        }
        if (!keep) {
            const kept = await admin.deleteCreated();
            if (kept.length > 0) console.warn(`mcpjam scenarios: could not delete ${kept.join(", ")}`);
        }
        stopLocal();
    };
}

function remoteConnection(): Connection {
    const url = required("OKF_MCP_URL");
    return {
        mode: "remote",
        url,
        apiBase: process.env.OKF_MCP_API_URL ?? `${new URL(url).origin}/api/v1`,
        key: required("OKF_MCP_KEY"),
    };
}

function required(name: string): string {
    const value = process.env[name];
    if (value === undefined || value === "") throw new Error(`${name} is required`);
    return value;
}

/**
 * Creates one catalog per scenario if the isolation allows it and the key may;
 * else falls back to one shared catalog.
 */
async function chooseCatalogs(
    admin: CatalogAdmin,
    connection: Connection,
): Promise<Pick<Target, "isolation" | "catalogs">> {
    const requested = process.env.OKF_MCP_ISOLATION ?? "auto";
    if (!["auto", "catalog", "folder"].includes(requested)) {
        throw new Error(`OKF_MCP_ISOLATION must be auto, catalog or folder, not ${requested}`);
    }
    const scenarios: Scenario[] = Object.values(SCENARIOS);
    const nameOf = (scenario: Scenario): string => `${admin.prefix}-${scenario.slug}`;

    if (requested !== "folder") {
        const owner = await admin.owner();
        const [first, ...rest] = scenarios;
        if (first === undefined) throw new Error("no scenarios");
        // The first create doubles as the capability probe.
        const refusal = await admin.create(nameOf(first), owner);
        if (refusal === null) {
            await Promise.all(rest.map((scenario) => admin.create(nameOf(scenario), owner)));
            return {
                isolation: "catalog",
                catalogs: Object.fromEntries(scenarios.map((s) => [s.slug, nameOf(s)])),
            };
        }
        if (requested === "catalog") {
            throw new Error(
                "OKF_MCP_ISOLATION=catalog, but the key may not create catalogs: use an admin " +
                    "key that reaches every catalog (`key create ... --access admin --all`)",
            );
        }
    }

    // Folder isolation: one catalog for everyone. Locally there is none to
    // name, so the run makes one (and deletes it at the end).
    let shared = process.env.OKF_MCP_CATALOG;
    if (shared === undefined || shared === "") {
        if (connection.mode === "remote") {
            throw new Error(
                "the key may not create catalogs, so OKF_MCP_CATALOG must name a scratch catalog",
            );
        }
        shared = `${admin.prefix}-shared`;
        await admin.create(shared, await admin.owner());
    }
    const catalog = shared;
    return {
        isolation: "folder",
        catalogs: Object.fromEntries(scenarios.map((s) => [s.slug, catalog])),
    };
}

interface Local {
    connection: Connection;
    server: ChildProcess;
}

async function startLocal(runId: string): Promise<Local> {
    const databaseUrl = process.env.DATABASE_URL ?? process.env.TEST_PG_URL;
    if (databaseUrl === undefined) {
        throw new Error(
            "set OKF_MCP_URL and OKF_MCP_KEY for a remote target, or DATABASE_URL / " +
                "TEST_PG_URL for a local one (`just test-mcpjam` starts one)",
        );
    }
    const bin = process.env.OKF_MCP_BIN ?? resolve(REPO_ROOT, "target/debug/okf-mcp");
    const env = { ...process.env, DATABASE_URL: databaseUrl };
    const cli = async (...args: string[]): Promise<string> =>
        (await run(bin, args, { env })).stdout.trim();

    // Names unique to this run, so a reused Postgres (TEST_PG_URL) is fine.
    // The key is the same kind staging needs for catalog isolation.
    const name = `mcpjam-${runId}`;
    await cli("migrate");
    await cli("user", "create", name);
    const key = await cli("key", "create", name, "--access", "admin", "--all", "--user", name);

    const port = await freePort();
    const server = spawn(bin, ["serve", "--listen", `127.0.0.1:${port}`], {
        // Warnings and errors only, unless the caller asks for more.
        env: { ...env, RUST_LOG: process.env.RUST_LOG ?? "warn" },
        stdio: ["ignore", "ignore", "inherit"],
    });
    const base = `http://127.0.0.1:${port}`;
    await waitHealthy(`${base}/healthz`, server);
    return {
        connection: { mode: "local", url: `${base}/mcp`, apiBase: `${base}/api/v1`, key },
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

// One publish per scenario, in its catalog. With their own catalogs they run
// in parallel; sharing one, in order, so the seeding itself never races.
async function seed(target: Target): Promise<void> {
    const scenarios = Object.values(SCENARIOS);
    if (target.isolation === "catalog") {
        await Promise.all(scenarios.map((scenario) => seedOne(target, scenario)));
    } else {
        for (const scenario of scenarios) await seedOne(target, scenario);
    }
}

async function seedOne(target: Target, scenario: Scenario): Promise<void> {
    const catalog = target.catalogs[scenario.slug];
    if (catalog === undefined) throw new Error(`no catalog for ${scenario.slug}`);
    const session = await OkfSession.open(target, catalog);
    try {
        const changeset = await session.openChangeset();
        for (const seed of scenario.seeds) {
            await session.call("create_concept", {
                path: `${folderOf(target.runFolder, scenario)}/${seed.path}`,
                frontmatter: { type: seed.type, title: seed.title, description: seed.description },
                body: seed.body,
                index_summary: seed.description,
                changeset,
            });
        }
        await session.publish(changeset, `mcpjam scenarios: seed ${scenario.slug}`);
    } finally {
        await session.close();
    }
}

// Folder isolation, remote: delete the run's concepts. index.md files are
// never deleted (the server keeps them), so an empty folder index per
// scenario remains.
async function cleanUpFolder(target: Target): Promise<void> {
    const catalog = Object.values(target.catalogs)[0];
    if (catalog === undefined) return;
    const session = await OkfSession.open(target, catalog);
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
