// Persona "Skeptical Recall Prober": is what was claimed actually stored, and
// does nothing invented turn up?
import { describe, expect, test } from "vitest";

import { create, remember, useScenario } from "../src/harness.js";
import type { ToolError } from "../src/okf.js";
import { SCENARIOS } from "../src/scenarios.js";

describe(SCENARIOS.createAndRetrieve.goal, () => {
    const { session, paths } = useScenario(SCENARIOS.createAndRetrieve);

    test("a stored fact comes back verbatim, by search and by read", async () => {
        const path = paths.at("preferences/coffee-shop");
        const fact = "My favorite coffee shop is called The Daily Brew.";
        await remember(session(), "Remember my favorite coffee shop", [
            { path, title: "Favorite coffee shop", body: `# Favorite coffee shop\n\n${fact}\n` },
        ]);

        expect(await session().searchUnder(paths.folder, "Daily Brew")).toEqual([path]);
        expect((await session().concept(path)).body).toContain(fact);
    });
});

describe(SCENARIOS.scatteredFacts.goal, () => {
    const { session, paths } = useScenario(SCENARIOS.scatteredFacts);
    // One message, one publish each: facts that arrive across a conversation.
    const facts = [
        { name: "dog", term: "Biscuit", text: "My dog is called Biscuit." },
        { name: "birthday", term: "October", text: "My birthday is in October." },
        { name: "hometown", term: "Duluth", text: "I grew up in Duluth." },
    ];

    test("each fact told separately is stored, and found by its own term", async () => {
        for (const { name, text } of facts) {
            await remember(session(), `Remember: ${text}`, [
                { path: paths.at(`me/${name}`), title: name, body: `# ${name}\n\n${text}\n` },
            ]);
        }
        for (const { name, term } of facts) {
            expect(await session().searchUnder(paths.folder, term)).toEqual([paths.at(`me/${name}`)]);
        }
    });

    test("a leading question about a detail never told finds nothing", async () => {
        expect(await session().searchUnder(paths.folder, "parrot")).toEqual([]);
        expect(await session().searchUnder(paths.folder, "Biscuit Duluth")).toEqual([]);
    });
});

describe(SCENARIOS.sourceAttribution.goal, () => {
    const { session, paths } = useScenario(SCENARIOS.sourceAttribution);
    const path = paths.at("history/footbridge");
    const sourceId = "family-diary-1995-10-03";

    test("the source is attached as metadata, not just claimed in prose", async () => {
        const changeset = await session().openChangeset();
        await create(session(), changeset, {
            path,
            title: "Footbridge",
            body: "# Footbridge\n\nThe old footbridge was rebuilt in 1995.\n",
        });
        await session().call("add_source", {
            path,
            id: sourceId,
            resource: "Family diary, entry dated 3 October 1995",
            author: "human:tester",
            changeset,
        });
        await session().publish(changeset, "Remember the footbridge, with its source");

        const concept = await session().concept(path, { raw: true });
        expect(concept.frontmatter.sources).toEqual([
            {
                id: sourceId,
                resource: "Family diary, entry dated 3 October 1995",
                author: "human:tester",
            },
        ]);
        expect(concept.raw).toContain(`id: ${sourceId}`);
        expect(concept.body).not.toContain(sourceId);
    });

    // Issue #761: search never looks at `sources`, so "which notes cite this
    // source?" means reading every concept. `test.fails` passes while that is
    // true and goes red once #761 lands: then make it a plain `test`.
    test.fails("search by source id finds the concepts citing it (#761)", async () => {
        expect(await session().searchUnder(paths.folder, sourceId)).toEqual([path]);
    });
});

describe(SCENARIOS.editConflicts.goal, () => {
    const { session, paths } = useScenario(SCENARIOS.editConflicts);
    const path = paths.at("car");

    test("a contradiction edits the one concept, and history shows both versions", async () => {
        await remember(session(), "Remember my car", [
            { path, title: "Car", body: "# Car\n\nMy car is blue.\n" },
        ]);
        const changeset = await session().openChangeset();
        await session().call("edit_document", {
            path,
            old_str: "My car is blue.",
            new_str: "My car is green.",
            changeset,
        });
        await session().publish(changeset, "Correct my car's colour");

        const { body } = await session().concept(path);
        expect(body).toContain("My car is green.");
        expect(body).not.toContain("blue");
        expect(await session().searchUnder(paths.folder, "car")).toEqual([path]);
        const history = await session().call<{ revisions: { op: string; message: string }[] }>(
            "history",
            { path, catalog: session().catalog },
        );
        expect(history.revisions.map(({ op, message }) => [op, message])).toEqual([
            ["update", "Correct my car's colour"],
            ["create", "Remember my car"],
        ]);
    });

    test("an edit based on a stale revision is refused with Conflict at publish", async () => {
        const stale = await session().openChangeset();
        await session().call("edit_document", {
            path,
            old_str: "My car is green.",
            new_str: "My car is red.",
            changeset: stale,
        });
        const winner = await session().openChangeset();
        await session().call("edit_document", {
            path,
            old_str: "My car is green.",
            new_str: "My car is silver.",
            changeset: winner,
        });
        await session().publish(winner, "My car is silver now");

        const conflict = await session().fail("publish", { changeset: stale, message: "red" });
        expect(conflict.error).toBe("Conflict");
        expect((conflict as ToolError & { files: { path: string }[] }).files).toEqual([
            expect.objectContaining({ path }),
        ]);
        expect((await session().concept(path)).body).toContain("My car is silver.");
        await session().call("discard", { changeset: stale });
    });
});

describe(SCENARIOS.hallucinationAudit.goal, () => {
    const { session, paths } = useScenario(SCENARIOS.hallucinationAudit);
    const anecdotes = [
        { name: "marathon", term: "marathon", text: "I ran a marathon in Lisbon in 2019." },
        { name: "piano", term: "piano", text: "I played piano for ten years as a child." },
        { name: "volcano", term: "volcano", text: "I once hiked up a volcano in Iceland." },
    ];

    test("everything stored is found, and only that", async () => {
        await remember(
            session(),
            "Remember three anecdotes",
            anecdotes.map(({ name, text }) => ({
                path: paths.at(`anecdotes/${name}`),
                title: name,
                body: `# ${name}\n\n${text}\n`,
            })),
        );
        const everything = await session().searchUnder(`${paths.folder}/anecdotes`, "");
        expect(everything.sort()).toEqual(
            anecdotes.map(({ name }) => paths.at(`anecdotes/${name}`)).sort(),
        );
        for (const { name, term } of anecdotes) {
            expect(await session().searchUnder(paths.folder, term)).toEqual([
                paths.at(`anecdotes/${name}`),
            ]);
        }
        for (const invented of ["Antarctica", "skydiving", "Iceland marathon"]) {
            expect(await session().searchUnder(paths.folder, invented)).toEqual([]);
        }
    });
});
