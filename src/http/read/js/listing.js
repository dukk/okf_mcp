// One folder of a catalog's tree (GET /catalogs/{c}/tree's documents:
// [{ path: '/a/b.md', kind, title, description }]): its index, its log, its
// subfolders (each with its own index's title and description, and how many
// concepts it holds, nested ones included) and its concepts. Machinery
// files are left out of the reader.

// A document's name in a list: its title, else its file name.
export function label(doc) {
    return doc.title ?? doc.path.split('/').pop();
}

export function folderListing(documents, folder) {
    const prefix = folder ? `/${folder}/` : '/';
    const folders = new Map();
    const concepts = [];
    let index = null;
    let log = null;
    const subfolder = name => {
        if (!folders.has(name)) {
            folders.set(name, { name, path: folder ? `${folder}/${name}` : name, title: null, description: null, pages: 0 });
        }
        return folders.get(name);
    };
    for (const doc of documents) {
        if (!doc.path.startsWith(prefix)) continue;
        const rest = doc.path.slice(prefix.length);
        const slash = rest.indexOf('/');
        if (slash >= 0) {
            const entry = subfolder(rest.slice(0, slash));
            if (doc.kind === 'concept') entry.pages += 1;
            // The subfolder's own index (not a deeper one) names it.
            else if (doc.kind === 'index' && rest.slice(slash + 1) === 'index.md') {
                entry.title = doc.title ?? null;
                entry.description = doc.description ?? null;
            }
        } else if (doc.kind === 'index') index = doc;
        else if (doc.kind === 'log') log = doc;
        else if (doc.kind === 'concept') concepts.push(doc);
    }
    concepts.sort((a, b) => label(a).localeCompare(label(b)));
    const sorted = [...folders.values()].sort((a, b) => (a.title ?? a.name).localeCompare(b.title ?? b.name));
    return { folders: sorted, index, log, concepts, label };
}

// Every folder's title by its path ('a/b'), from the folders' indexes, for
// breadcrumbs. The root's index names the catalog's page, not a folder.
export function folderTitles(documents) {
    const titles = new Map();
    for (const doc of documents) {
        if (doc.kind !== 'index' || !doc.title) continue;
        const folder = doc.path.replace(/^\/+/, '').split('/').slice(0, -1).join('/');
        if (folder) titles.set(folder, doc.title);
    }
    return titles;
}
