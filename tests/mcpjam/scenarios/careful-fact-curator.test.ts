// Persona "Careful Fact Curator": sourcing, drafts, folders and stamps.
import { describe, expect, test } from "vitest";

import { create, useScenario } from "../src/harness.js";
import type { SourceEntry, Stamp } from "../src/okf.js";
import { SCENARIOS } from "../src/scenarios.js";

describe(SCENARIOS.createAndSource.goal, () => {
    const { session, paths } = useScenario(SCENARIOS.createAndSource);

    test("a concept created and sourced in one changeset publishes with its source", async () => {
        const path = paths.at("history/first-bike");
        const changeset = await session().openChangeset();
        await create(session(), changeset, {
            path,
            title: "First bike",
            body: "# First bike\n\nI got my first bike, a red one, in 1988.\n",
            summary: "My first bike, a red one, in 1988.",
        });
        // Sequential, after create_concept returned: the draft is visible to
        // the next write in the same changeset (the swarm's NotFound came from
        // sending both at once).
        await session().call("add_source", {
            path,
            id: "photo-album-1988",
            resource: "Family photo album, 1988",
            title: "Photo album",
            author: "human:tester",
            changeset,
        });
        await session().publish(changeset, "Remember my first bike, with its source");

        const concept = await session().concept(path);
        expect(concept.frontmatter.sources).toEqual([
            {
                id: "photo-album-1988",
                resource: "Family photo album, 1988",
                title: "Photo album",
                author: "human:tester",
            },
        ]);
    });

    test("a source on a concept that does not exist is NotFound, naming the path", async () => {
        const path = paths.at("history/never-written");
        const changeset = await session().openChangeset();
        const error = await session().fail("add_source", {
            path,
            id: "nothing",
            resource: "nowhere",
            changeset,
        });
        expect(error.error).toBe("NotFound");
        expect(error.message).toContain(path);
        await session().call("discard", { changeset });
    });
});

describe(SCENARIOS.reviewAndPublish.goal, () => {
    const { session, paths } = useScenario(SCENARIOS.reviewAndPublish);

    test("a draft is visible through its changeset only, until publish", async () => {
        const path = paths.at("garden-plan");
        const changeset = await session().openChangeset();
        await create(session(), changeset, {
            path,
            title: "Garden plan",
            body: "# Garden plan\n\nTomatoes go along the south fence.\n",
        });

        const draft = await session().concept(path, { changeset });
        expect(draft.body).toContain("Tomatoes go along the south fence.");
        const unpublished = await session().fail("get_concept", {
            path,
            catalog: session().catalog,
        });
        expect(unpublished.error).toBe("NotFound");

        const pending = await session().call<{ files: { path: string; change: string }[] }>(
            "pending_changes",
            { changeset },
        );
        expect(pending.files).toContainEqual(expect.objectContaining({ path, change: "create" }));

        const published = await session().publish(changeset, "Add the garden plan");
        expect(published.published).toBe(true);
        expect((await session().concept(path)).body).toBe(draft.body);

        const history = await session().call<{ revisions: { op: string; message: string }[] }>(
            "history",
            { path, catalog: session().catalog },
        );
        expect(history.revisions).toEqual([
            expect.objectContaining({ op: "create", message: "Add the garden plan" }),
        ]);
    });
});

describe(SCENARIOS.addContextWithSources.goal, () => {
    const { session, paths } = useScenario(SCENARIOS.addContextWithSources);
    const path = paths.at("history/old-footbridge");

    test("several sources on an existing concept keep their order; a duplicate id is refused", async () => {
        const changeset = await session().openChangeset();
        const sources: SourceEntry[] = [
            { id: "diary-1995-10-03", resource: "Family diary, 3 October 1995" },
            { id: "council-minutes-1995", resource: "Town council minutes, 1995" },
            { id: "plaque", resource: "Plaque on the bridge's east end" },
        ];
        for (const source of sources) {
            await session().call("add_source", { path, ...source, changeset });
        }
        const duplicate = await session().fail("add_source", {
            path,
            id: "plaque",
            resource: "the same plaque again",
            changeset,
        });
        expect(duplicate.error).toBe("Refused");

        await session().publish(changeset, "Source the footbridge note three ways");
        expect((await session().concept(path)).frontmatter.sources).toEqual(sources);
    });
});

describe(SCENARIOS.organizeIntoFolders.goal, () => {
    const { session, paths } = useScenario(SCENARIOS.organizeIntoFolders);

    test("concepts created with summaries are listed in a new folder index", async () => {
        const concepts = [
            { name: "primary-sources", summary: "What counts as a primary source." },
            { name: "corroboration", summary: "Checking one source against another." },
            { name: "provenance", summary: "Where a document came from and who held it." },
        ];
        const changeset = await session().openChangeset();
        for (const { name, summary } of concepts) {
            await create(session(), changeset, {
                path: paths.at(`evidence/${name}`),
                title: name,
                description: summary,
                body: `# ${name}\n\n${summary}\n`,
                summary,
            });
        }
        await session().publish(changeset, "Add the evidence evaluation concepts");

        const folderIndex = await session().document(`${paths.folder}/evidence/index.md`);
        for (const { name, summary } of concepts) {
            expect(folderIndex).toContain(`(${name}.md) - ${summary}`);
        }
        const parentIndex = await session().document(`${paths.folder}/index.md`);
        expect(parentIndex).toContain("evidence/");
    });
});

describe(SCENARIOS.verifyAndStamp.goal, () => {
    const { session, paths } = useScenario(SCENARIOS.verifyAndStamp);
    const path = paths.at("infra/ntp");

    test("review and verified stamp record who and when, with the server's clock", async () => {
        const before = Date.now();
        const changeset = await session().openChangeset();
        await session().call("review", {
            path,
            outcome: "verified",
            note: "Checked against my own hosts' sync logs.",
            by: "human:tester",
            changeset,
        });
        await session().call("stamp", { path, field: "verified", by: "human:tester", changeset });
        await session().publish(changeset, "Verify the NTP concept");
        const after = Date.now();

        const { frontmatter } = await session().concept(path);
        const reviewed = frontmatter.reviewed as Stamp;
        expect(reviewed).toMatchObject({
            by: "human:tester",
            outcome: "verified",
            note: "Checked against my own hosts' sync logs.",
        });
        const verified = frontmatter.verified as Stamp[];
        expect(verified).toEqual([expect.objectContaining({ by: "human:tester" })]);
        // Generous: the target's clock may differ from this machine's a little.
        for (const stamp of [reviewed, ...verified]) {
            const at = Date.parse(stamp.at);
            expect(at).toBeGreaterThan(before - 5 * 60_000);
            expect(at).toBeLessThan(after + 5 * 60_000);
        }
    });

    test("stamping a concept that does not exist is NotFound", async () => {
        const changeset = await session().openChangeset();
        const error = await session().fail("stamp", {
            path: paths.at("infra/no-such-system"),
            field: "verified",
            changeset,
        });
        expect(error.error).toBe("NotFound");
        await session().call("discard", { changeset });
    });
});
