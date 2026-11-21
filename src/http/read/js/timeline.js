// Home's activity timeline (reader polish design, "Home"): recent changes
// (newest first, as GET /catalogs/{c}/recent-changes lists them) grouped by
// the device's local day, then into batches: consecutive changes by the
// same actor with the same message, each less than BATCH_GAP_MS from the
// one before. Filters apply first. Presentational only, no DOM, so Node
// tests it (tests/read_worker/timeline.test.mjs).
import { docLabel, pages } from './text.js';

export const BATCH_GAP_MS = 2 * 60 * 1000;
// How many of a group's documents show before "+N more".
export const GROUP_PREVIEW = 3;

export const FILTERS = Object.freeze([
    { id: 'all', label: 'All' },
    { id: 'created', label: 'Created' },
    { id: 'edited', label: 'Edited' },
    { id: 'deleted', label: 'Deleted' },
    { id: 'mine', label: 'By me' },
]);

// What a change did to its document: 'new' (its first revision), 'edited'
// or 'deleted'. An import is new or edited by its revision too.
export function changeKind(change) {
    if (change.op === 'delete') return 'deleted';
    return change.revision === 1 ? 'new' : 'edited';
}

// Whether `change` shows under `filter` (a FILTERS id) for the user `me`
// (the session's user name, or null when unknown).
function matches(change, filter, me) {
    switch (filter) {
        case 'created': return changeKind(change) === 'new';
        case 'edited': return changeKind(change) === 'edited';
        case 'deleted': return changeKind(change) === 'deleted';
        case 'mine': return me !== null && change.actor === me;
        default: return true;
    }
}

// The local calendar day of `when` (an ISO time or a Date), 'YYYY-MM-DD'.
export function dayKey(when) {
    const date = new Date(when);
    const pad = n => String(n).padStart(2, '0');
    return `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())}`;
}

// 'Today', 'Yesterday', else the date ('Wed, Nov 18').
export function dayLabel(key, now = new Date()) {
    if (key === dayKey(now)) return 'Today';
    const yesterday = new Date(now);
    yesterday.setDate(yesterday.getDate() - 1);
    if (key === dayKey(yesterday)) return 'Yesterday';
    const [year, month, day] = key.split('-').map(Number);
    return new Date(year, month - 1, day).toLocaleDateString('en-US', { weekday: 'short', month: 'short', day: 'numeric' });
}

// Whether `change` belongs to the batch `group` ends with.
function continues(group, change) {
    const last = group.items[group.items.length - 1];
    return last.actor === change.actor && last.message === change.message
        && Math.abs(Date.parse(last.at) - Date.parse(change.at)) < BATCH_GAP_MS;
}

// The timeline of `changes`: [{ key, label, groups: [{ actor, actorLabel,
// at, items }] }], days and groups newest first. `filter` is a FILTERS id,
// `me` the session's user name, `now` the device's time.
export function timeline(changes, { filter = 'all', me = null, now = new Date() } = {}) {
    const days = [];
    for (const change of changes ?? []) {
        if (!matches(change, filter, me)) continue;
        const key = dayKey(change.at);
        let day = days[days.length - 1];
        if (!day || day.key !== key) {
            day = { key, label: dayLabel(key, now), groups: [] };
            days.push(day);
        }
        const group = day.groups[day.groups.length - 1];
        if (group && continues(group, change)) group.items.push(change);
        else day.groups.push({ actor: change.actor, actorLabel: change.actor_label ?? null, at: change.at, items: [change] });
    }
    return days;
}

const VERBS = Object.freeze({ create: 'created', update: 'edited', delete: 'deleted', import: 'imported' });

// What a group did: 'imported', 'edited', ..., or 'changed' for mixed ops.
export function verb(group) {
    const ops = new Set(group.items.map(change => change.op));
    const [only] = ops;
    return ops.size === 1 ? (VERBS[only] ?? 'changed') : 'changed';
}

// What it did it to: the document's name for one, else 'N pages'.
export function object(group) {
    return group.items.length === 1 ? docLabel(group.items[0]) : pages(group.items.length);
}

// 'imported 6 pages', 'edited Kubernetes cluster'.
export function summary(group) {
    return `${verb(group)} ${object(group)}`;
}
