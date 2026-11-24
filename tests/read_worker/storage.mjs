// In-memory Cache Storage shared by the reader's Node tests (sw.test.mjs and
// page.test.mjs): one Storage outlives every worker or page that opens it,
// and deleting a cache orphans every handle to it, as in a browser.

/** The origin every test's URLs resolve against. */
export const ORIGIN = 'https://reader.example.com';

/** Cache Storage that outlives a worker, so a restart sees what was kept. */
export class Storage {
    constructor() {
        this.caches = new Map();
        // `refuse(op, cacheName)` true makes that call reject.
        this.refuse = () => false;
        // Awaited inside every put, so a test can act mid-write.
        this.beforePut = async () => {};
    }

    check(op, name) {
        if (this.refuse(op, name)) throw new DOMException('refused', 'SecurityError');
    }

    /** The CacheStorage object one worker sees. */
    api() {
        const storage = this;
        const tick = () => new Promise(resolve => setImmediate(resolve));
        const urlOf = key => (typeof key === 'string' ? new URL(key, ORIGIN).href : key.url);
        class Cache {
            constructor(name) { this.name = name; this.entries = []; }
            async match(key) {
                storage.check('match', this.name);
                const found = this.entries.find(entry => entry.url === urlOf(key));
                return found ? found.response.clone() : undefined;
            }
            async put(key, response) {
                storage.check('put', this.name);
                const body = await response.arrayBuffer();
                await storage.beforePut(this.name, urlOf(key));
                await tick();
                const url = urlOf(key);
                this.entries = this.entries.filter(entry => entry.url !== url);
                this.entries.push({ url, response: new Response(body, { status: response.status, headers: response.headers }) });
            }
            async delete(key) {
                storage.check('delete', this.name);
                const before = this.entries.length;
                this.entries = this.entries.filter(entry => entry.url !== urlOf(key));
                return before !== this.entries.length;
            }
            async keys() {
                storage.check('keys', this.name);
                return this.entries.map(entry => new Request(entry.url));
            }
        }
        return {
            async open(name) {
                await tick();
                storage.check('open', name);
                if (!storage.caches.has(name)) storage.caches.set(name, new Cache(name));
                return storage.caches.get(name);
            },
            async delete(name) {
                await tick();
                storage.check('delete', name);
                return storage.caches.delete(name);
            },
            async keys() {
                storage.check('keys', null);
                return [...storage.caches.keys()];
            },
            async match(key, options) {
                storage.check('match', options?.cacheName);
                return storage.caches.get(options?.cacheName)?.match(key);
            },
        };
    }

    paths(name) {
        return (this.caches.get(name)?.entries ?? []).map(entry => new URL(entry.url).pathname);
    }

    async ownerTag() {
        const record = await this.caches.get('read-owner')?.match('/read/__owner');
        return record ? (await record.json()).tag : undefined;
    }
}
