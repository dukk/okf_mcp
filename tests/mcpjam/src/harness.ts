// Shared setup for the scenario files: a session per file, and paths that
// stay inside a scenario's folder.
import { afterAll, beforeAll, inject } from "vitest";

import { OkfSession, type Target } from "./okf.js";
import { folderOf, type Scenario } from "./scenarios.js";

/** The target this run uses (see global-setup.ts). */
export function target(): Target {
    return inject("target");
}

/** The catalog `scenario` writes in: its own, or the shared one. */
export function catalogOf(scenario: Scenario): string {
    const catalog = target().catalogs[scenario.slug];
    if (catalog === undefined) throw new Error(`no catalog for ${scenario.slug}`);
    return catalog;
}

/** A scenario's session getter and paths, for one `describe`. */
export interface ScenarioContext {
    /** The scenario's MCP session, bound to its catalog; valid from the first test on. */
    session: () => OkfSession;
    paths: Paths;
}

/**
 * Opens one MCP session for a `describe`'s tests, in the scenario's catalog,
 * and closes it afterwards. Call it inside the `describe`.
 */
export function useScenario(scenario: Scenario): ScenarioContext {
    let session: OkfSession | undefined;
    beforeAll(async () => {
        session = await OkfSession.open(target(), catalogOf(scenario));
    });
    afterAll(async () => {
        await session?.close();
    });
    return {
        session: () => {
            if (session === undefined) throw new Error("useScenario: no session before beforeAll ran");
            return session;
        },
        paths: pathsOf(scenario),
    };
}

/** Builds paths inside one scenario's folder. */
export interface Paths {
    /** The scenario's folder, e.g. `/swarm-x/quiz`. */
    folder: string;
    /** A concept path in it: `at("pets/dog")` is `/swarm-x/quiz/pets/dog.md`. */
    at(relative: string): string;
}

/** The paths of `scenario` in this run. */
export function pathsOf(scenario: Scenario): Paths {
    const folder = folderOf(target().runFolder, scenario);
    return { folder, at: (relative) => `${folder}/${relative}.md` };
}

/** The fields a scenario writes into a new concept. */
export interface NewConcept {
    path: string;
    title: string;
    body: string;
    description?: string;
    type?: string;
    summary?: string;
}

/**
 * Creates one concept in `changeset` (listed in its folder index when it has a
 * `summary`).
 */
export async function create(
    session: OkfSession,
    changeset: string,
    concept: NewConcept,
): Promise<void> {
    await session.call("create_concept", {
        path: concept.path,
        frontmatter: {
            type: concept.type ?? "Note",
            title: concept.title,
            ...(concept.description === undefined ? {} : { description: concept.description }),
        },
        body: concept.body,
        ...(concept.summary === undefined ? {} : { index_summary: concept.summary }),
        changeset,
    });
}

/** Creates the concepts in a changeset of their own and publishes them. */
export async function remember(
    session: OkfSession,
    message: string,
    concepts: NewConcept[],
): Promise<void> {
    const changeset = await session.openChangeset();
    for (const concept of concepts) {
        await create(session, changeset, concept);
    }
    await session.publish(changeset, message);
}
