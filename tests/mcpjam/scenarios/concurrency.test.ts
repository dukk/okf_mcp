// Several MCP sessions on one catalog at once: the races the 2026-11-19 swarm
// wave hit, when every persona shared one catalog. Unlike the persona files,
// these tests collide on purpose: several sessions, one folder, sometimes one
// changeset. Against a multi-replica staging, the sessions can also land on
// different replicas.
import { afterAll, beforeAll, describe, expect, test } from "vitest";

import { create, pathsOf, target } from "../src/harness.js";
import { OkfSession, type Outcome, type SourceEntry } from "../src/okf.js";
import { SCENARIOS } from "../src/scenarios.js";

const SESSIONS = 8;
const paths = pathsOf(SCENARIOS.concurrency);
let sessions: OkfSession[] = [];

beforeAll(async () => {
    sessions = await Promise.all(Array.from({ length: SESSIONS }, () => OkfSession.open(target())));
});
afterAll(async () => {
    await Promise.all(sessions.map((session) => session.close()));
});

/** Session `n` (every test has at least two). */
function session(n: number): OkfSession {
    const chosen = sessions[n];
    if (chosen === undefined) throw new Error(`no session ${n}`);
    return chosen;
}

describe("dependent calls sent in parallel on one changeset", () => {
    // The swarm's add_source "NotFound" finding: create_concept and add_source
    // sent in one turn. Writes to a changeset are queued, so each is valid on
    // its own, but their order is whichever arrives first.
    test("each call answers validly for the order it ran in, and a retry after succeeds", async () => {
        for (let round = 0; round < 5; round += 1) {
            const path = paths.at(`parallel/round-${round}`);
            const changeset = await session(0).openChangeset();
            const [created, sourced] = await Promise.all([
                session(0).attempt("create_concept", {
                    path,
                    frontmatter: { type: "Note", title: `Round ${round}` },
                    body: `# Round ${round}\n`,
                    changeset,
                }),
                session(0).attempt("add_source", {
                    path,
                    id: "s1",
                    resource: "a source",
                    changeset,
                }),
            ]);
            expect(created.ok).toBe(true);
            if (!sourced.ok) {
                // add_source ran first: the only valid failure, naming the path.
                expect(sourced.error.error).toBe("NotFound");
                expect(sourced.error.message).toContain(path);
                await session(0).call("add_source", { path, id: "s1", resource: "a source", changeset });
            }
            await session(0).publish(changeset, `Round ${round}`);
            expect((await session(0).concept(path)).frontmatter.sources).toEqual([
                { id: "s1", resource: "a source" },
            ]);
        }
    });
});

describe("many sessions writing to one changeset", () => {
    test("every concurrent write lands; none is lost to another", async () => {
        const path = paths.at("fan-out/target");
        const changeset = await session(0).openChangeset();
        await create(session(0), changeset, { path, title: "Fan-out", body: "# Fan-out\n" });

        // The handle is the changeset, whichever session holds it.
        const sources: SourceEntry[] = sessions.map((_, n) => ({
            id: `source-${n}`,
            resource: `written by session ${n}`,
        }));
        const outcomes = await Promise.all(
            sources.map((source, n) => session(n).attempt("add_source", { path, ...source, changeset })),
        );
        expect(outcomes.filter((outcome) => !outcome.ok)).toEqual([]);

        await session(0).publish(changeset, "Fan-out from every session");
        const stored = (await session(1).concept(path)).frontmatter.sources as SourceEntry[];
        expect(stored.map(({ id }) => id).sort()).toEqual(sources.map(({ id }) => id).sort());
    });
});

describe("sessions publishing into one folder at once", () => {
    async function createInOwnChangeset(n: number, path: string): Promise<string> {
        const changeset = await session(n).openChangeset();
        await create(session(n), changeset, {
            path,
            title: `From session ${n}`,
            body: `# From session ${n}\n`,
            summary: `Written by session ${n}.`,
        });
        return changeset;
    }

    test("index listings in an existing folder are replayed, so every publish succeeds", async () => {
        const folder = `${paths.folder}/shared`;
        const changesets = await Promise.all(
            sessions.map((_, n) => createInOwnChangeset(n, `${folder}/existing-${n}.md`)),
        );
        const outcomes = await Promise.all(
            changesets.map((changeset, n) =>
                session(n).attempt("publish", { changeset, message: `Session ${n} adds a note` }),
            ),
        );
        expect(failures(outcomes)).toEqual([]);

        const index = await session(0).document(`${folder}/index.md`);
        for (let n = 0; n < SESSIONS; n += 1) expect(index).toContain(`(existing-${n}.md)`);
    });

    // The swarm's Quiz-session Conflict: two sessions creating the first
    // concept of the same new folder, so both create its index.md and list it
    // in the parent. Every publish should land, like the existing-folder case.
    test("first concepts of the same new folder from two sessions both publish", async () => {
        const folder = `${paths.folder}/brand-new`;
        const changesets = await Promise.all([
            createInOwnChangeset(0, `${folder}/first-a.md`),
            createInOwnChangeset(1, `${folder}/first-b.md`),
        ]);
        const outcomes = await Promise.all(
            changesets.map((changeset, n) =>
                session(n).attempt("publish", { changeset, message: `Session ${n} starts a folder` }),
            ),
        );
        expect(failures(outcomes)).toEqual([]);

        const index = await session(2).document(`${folder}/index.md`);
        expect(index).toContain("(first-a.md)");
        expect(index).toContain("(first-b.md)");
    });

    test("racing edits of the same line: exactly one publishes, the rest get Conflict", async () => {
        const path = `${paths.folder}/shared/readme.md`;
        const changesets = await Promise.all(
            sessions.map(async (_, n) => {
                const changeset = await session(n).openChangeset();
                await session(n).call("edit_document", {
                    path,
                    old_str: "The line every session edits.",
                    new_str: `Session ${n} won the edit.`,
                    changeset,
                });
                return changeset;
            }),
        );
        const outcomes = await Promise.all(
            changesets.map((changeset, n) =>
                session(n).attempt("publish", { changeset, message: `Session ${n} edits the line` }),
            ),
        );

        const winners = outcomes.flatMap((outcome, n) => (outcome.ok ? [n] : []));
        expect(winners).toHaveLength(1);
        for (const outcome of outcomes) {
            if (!outcome.ok) expect(outcome.error.error).toBe("Conflict");
        }
        const { body } = await session(0).concept(path);
        expect(body).toContain(`Session ${winners[0]} won the edit.`);
        await Promise.all(
            outcomes.map((outcome, n) =>
                outcome.ok ? undefined : session(n).call("discard", { changeset: changesets[n] }),
            ),
        );
    });
});

describe("reading another session's publish", () => {
    // The swarm read raced its own publish. Sequenced, a publish must be
    // visible at once to every session, whichever replica it reaches.
    test("a publish is visible to every other session as soon as it returns", async () => {
        const path = paths.at("visibility/note");
        const changeset = await session(0).openChangeset();
        await create(session(0), changeset, {
            path,
            title: "Visible",
            body: "# Visible\n\nPublished by session 0.\n",
            summary: "Published by session 0.",
        });
        await session(0).publish(changeset, "Publish for the others");

        const reads = await Promise.all(sessions.slice(1).map((other) => other.concept(path)));
        for (const read of reads) expect(read.body).toContain("Published by session 0.");
        const indexes = await Promise.all(
            sessions.slice(1).map((other) => other.document(`${paths.folder}/visibility/index.md`)),
        );
        for (const index of indexes) expect(index).toContain("(note.md)");
    });
});

/** The failed outcomes, labelled with their session, for a readable diff. */
function failures(outcomes: Outcome[]): string[] {
    return outcomes.flatMap((outcome, n) =>
        outcome.ok ? [] : [`session ${n}: ${outcome.error.error}: ${outcome.error.message}`],
    );
}
