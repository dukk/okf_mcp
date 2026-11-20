// One folder of a catalog's tree (GET /catalogs/{c}/tree's documents:
// [{ path: '/a/b.md', kind, title }]): its subfolders, its index and its
// concepts. Logs and machinery files are left out of the reader.

function label(doc) {
    return doc.title ?? doc.path.split('/').pop();
}

export function folderListing(documents, folder) {
    const prefix = folder ? `/${folder}/` : '/';
    const folders = new Set();
    const concepts = [];
    let index = null;
    for (const doc of documents) {
        if (!doc.path.startsWith(prefix)) continue;
        const rest = doc.path.slice(prefix.length);
        const slash = rest.indexOf('/');
        if (slash >= 0) folders.add(rest.slice(0, slash));
        else if (doc.kind === 'index') index = doc;
        else if (doc.kind === 'concept') concepts.push(doc);
    }
    concepts.sort((a, b) => label(a).localeCompare(label(b)));
    return { folders: [...folders].sort((a, b) => a.localeCompare(b)), index, concepts, label };
}
