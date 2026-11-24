// The reader's cache names and the concept cap, written once for both the
// page (index.html loads this as a classic script) and the worker (sw.js
// loads it with importScripts). `self` is the window in one and the worker
// in the other.
self.READ_CACHES = Object.freeze({
    shellPrefix: 'read-shell-',
    content: 'read-content',
    meta: 'read-meta',
    owner: 'read-owner',
    // The one entry of `owner`: { tag }, the account whose copies are cached.
    ownerKey: '/read/__owner',
    contentCap: 200,
});
