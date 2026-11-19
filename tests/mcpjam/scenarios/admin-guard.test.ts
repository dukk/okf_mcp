// The delete guard of CatalogAdmin: a key that may create catalogs may also
// delete any of them, so the suite must never touch one it did not create.
// No server is needed: the API base is unroutable, so any request would fail
// with a network error instead of the guard's refusal.
import { describe, expect, test } from "vitest";

import { CatalogAdmin } from "../src/admin.js";

const UNROUTABLE = "http://okf-mcp-admin-guard.invalid/api/v1";

describe("CatalogAdmin's guard", () => {
    test("refuses to create a catalog outside the run's prefix, before any request", async () => {
        const admin = new CatalogAdmin(UNROUTABLE, "no-key", "mcpjam-run1");
        await expect(admin.create("production", "team")).rejects.toThrow(/refusing production/);
        await expect(admin.create("mcpjam-run2-quiz", "team")).rejects.toThrow(/refusing/);
        // A bare prefix, without the `-<slug>`, is not one of the run's names.
        await expect(admin.create("mcpjam-run1", "team")).rejects.toThrow(/refusing/);
    });

    test("deletes nothing it did not create", async () => {
        const admin = new CatalogAdmin(UNROUTABLE, "no-key", "mcpjam-run1");
        expect(await admin.deleteCreated()).toEqual([]);
    });
});
