// Persona "Playful Memory Tester": casual facts saved and recalled later.
import { describe, expect, test } from "vitest";

import { remember, useScenario } from "../src/harness.js";
import { SCENARIOS } from "../src/scenarios.js";

/** A Q&A note body, as an assistant would save interview answers. */
function answers(title: string, pairs: [string, string][]): string {
    const lines = pairs.map(([question, answer]) => `- **${question}** ${answer}`);
    return `# ${title}\n\n${lines.join("\n")}\n`;
}

describe(SCENARIOS.quiz.goal, () => {
    const { session, paths } = useScenario(SCENARIOS.quiz);
    const path = paths.at("personal/life-answers");

    test("answers saved earlier are there to quiz on", async () => {
        await remember(session(), "Save answers to four quick questions", [
            {
                path,
                title: "Life answers",
                summary: "Answers to four quick personal questions.",
                body: answers("Life answers", [
                    ["Small happy thing:", "A bakery gave me a free cinnamon roll."],
                    ["City for a year:", "Lisbon, for the light and the old buildings."],
                    ["Skill to learn:", "Pottery."],
                    ["Last great meal:", "Spicy ramen with a soft-boiled egg."],
                ]),
            },
        ]);
        const { body } = await session().concept(path);
        for (const recalled of ["Lisbon", "Pottery", "cinnamon roll", "ramen"]) {
            expect(body).toContain(recalled);
        }
    });
});

describe(SCENARIOS.knowledgeBase.goal, () => {
    const { session, paths } = useScenario(SCENARIOS.knowledgeBase);
    const categories = ["foods", "travel", "family", "hobbies"];

    test("a collection per category, which a later session adds to", async () => {
        await remember(
            session(),
            "Start the personal knowledge base",
            categories.map((category) => ({
                path: paths.at(`memories/${category}`),
                title: category,
                summary: `My ${category}.`,
                body: `# ${category}\n\n- A first entry about ${category}.\n`,
            })),
        );
        const index = await session().document(`${paths.folder}/memories/index.md`);
        for (const category of categories) expect(index).toContain(`(${category}.md)`);

        const path = paths.at("memories/foods");
        const changeset = await session().openChangeset();
        await session().call("edit_document", {
            path,
            old_str: "- A first entry about foods.\n",
            new_str: "- A first entry about foods.\n- Mango sticky rice, since Bangkok.\n",
            changeset,
        });
        await session().publish(changeset, "Add a food memory");

        expect((await session().concept(path)).body).toContain("Mango sticky rice");
        const history = await session().call<{ revisions: unknown[] }>("history", {
            path,
            catalog: session().catalog,
        });
        expect(history.revisions).toHaveLength(2);
    });
});

describe(SCENARIOS.surpriseQuestions.goal, () => {
    const { session, paths } = useScenario(SCENARIOS.surpriseQuestions);
    const tidbits = [
        { name: "pet", text: "My cat is called Pickles.", question: "Pickles" },
        { name: "hobby", text: "I build model trains.", question: "trains" },
        { name: "city", text: "I lived in Montreal for two years.", question: "Montreal" },
    ];

    test("a tidbit mentioned in passing answers a surprise question later", async () => {
        for (const { name, text } of tidbits) {
            await remember(session(), `Note in passing: ${text}`, [
                { path: paths.at(`tidbits/${name}`), title: name, body: `# ${name}\n\n${text}\n` },
            ]);
        }
        for (const { name, text, question } of tidbits) {
            const [hit] = await session().searchUnder(paths.folder, question);
            expect(hit).toBe(paths.at(`tidbits/${name}`));
            expect((await session().concept(hit ?? "")).body).toContain(text);
        }
    });
});

describe(SCENARIOS.indexedArchive.goal, () => {
    const { session, paths } = useScenario(SCENARIOS.indexedArchive);
    const categories = ["family", "travel", "hobbies", "food"];

    test("one folder per category, each listed in the archive's index and searchable", async () => {
        await remember(
            session(),
            "Create the memory archive",
            categories.map((category) => ({
                path: paths.at(`${category}/first`),
                title: `First ${category} memory`,
                type: "Memory",
                summary: `The first ${category} memory.`,
                body: `# First ${category} memory\n\nSomething about ${category}.\n`,
            })),
        );
        const archiveIndex = await session().document(`${paths.folder}/index.md`);
        for (const category of categories) {
            expect(archiveIndex).toContain(`${category}/`);
            const folderIndex = await session().document(`${paths.folder}/${category}/index.md`);
            expect(folderIndex).toContain("(first.md)");
        }
        const { hits } = await session().call<{ hits: { path: string }[] }>("search", {
            query: "",
            type: "Memory",
            catalogs: [session().catalog],
            limit: 200,
        });
        const mine = hits.map((hit) => hit.path).filter((path) => path.startsWith(paths.folder));
        expect(mine.sort()).toEqual(categories.map((c) => paths.at(`${c}/first`)).sort());
    });
});

describe(SCENARIOS.captureInterview.goal, () => {
    const { session, paths } = useScenario(SCENARIOS.captureInterview);
    const path = paths.at("interview");

    test("one-liner answers persist without the user saying how to store them", async () => {
        const pairs: [string, string][] = [
            ["Favorite childhood food:", "Grilled cheese."],
            ["Pet's name:", "Rufus."],
            ["Memorable trip:", "Driving the coast road in Big Sur."],
        ];
        await remember(session(), "Capture the interview answers", [
            { path, title: "Interview", body: answers("Interview", pairs) },
        ]);
        const { body } = await session().concept(path);
        for (const [, answer] of pairs) expect(body).toContain(answer);
    });
});
