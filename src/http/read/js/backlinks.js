// "Linked from" (reader polish design, "Concept"): GET /links lists one
// backlink per link, so a document that links three times appears three
// times. Here it is one row with a count. No DOM, so Node tests it.
import { fileName } from './text.js';

// [{ path, title, count }], one per linking document, in first-seen order.
export function dedupeBacklinks(backlinks) {
    const rows = new Map();
    for (const link of backlinks ?? []) {
        const row = rows.get(link.path);
        if (row) row.count += 1;
        else rows.set(link.path, { path: link.path, title: link.title ?? null, count: 1 });
    }
    return [...rows.values()];
}

// The row's note: '3 links' for a document that links more than once,
// '6 entries' for a log (each entry is one link), else ''.
export function backlinkNote(row) {
    if (fileName(row.path) === 'log.md') return `${row.count} ${row.count === 1 ? 'entry' : 'entries'}`;
    return row.count > 1 ? `${row.count} links` : '';
}
