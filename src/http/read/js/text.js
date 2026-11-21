// Display text the screens share: names from paths, counts, ages. No DOM
// and no imports, so Node tests it (tests/read_worker/text.test.mjs).

// The segments of a catalog path ('/a/b.md' -> ['a', 'b.md']).
function segments(path) {
    return String(path ?? '').split('/').filter(Boolean);
}

// The file name of a catalog path ('/a/b.md' -> 'b.md').
export function fileName(path) {
    const parts = segments(path);
    return parts[parts.length - 1] ?? '';
}

// The folders above `path`, for display ('/a/b/c.md' -> 'a > b' with a
// chevron); '' at the root.
export function folderTrail(path) {
    return segments(path).slice(0, -1).join(' › ');
}

// What a document ({ path, title }) is called: its title, else its file name.
export function docLabel(doc) {
    return doc?.title || fileName(doc?.path);
}

// '1,284 pages', '1 page'.
export function pages(count) {
    const n = Number(count) || 0;
    return `${n.toLocaleString('en-US')} ${n === 1 ? 'page' : 'pages'}`;
}

// Characters of prose read in a minute: about 230 words of 5 letters and a space.
export const CHARS_PER_MINUTE = 1200;

// Whole minutes to read `chars` characters, at least 1.
export function readingMinutes(chars) {
    return Math.max(1, Math.round((Number(chars) || 0) / CHARS_PER_MINUTE));
}

// How long ago `iso` was, short: '27 s', '5 min', '3 h', '2 d'. `now` is
// milliseconds since the epoch (Date.now() by default).
export function shortAgo(iso, now = Date.now()) {
    const seconds = Math.max(0, Math.round((now - Date.parse(iso)) / 1000));
    if (seconds < 60) return `${seconds} s`;
    if (seconds < 3600) return `${Math.floor(seconds / 60)} min`;
    if (seconds < 86400) return `${Math.floor(seconds / 3600)} h`;
    return `${Math.floor(seconds / 86400)} d`;
}

// The local time of `iso` on the 24-hour clock ('16:20').
export function clock(iso) {
    const date = new Date(iso);
    const pad = n => String(n).padStart(2, '0');
    return `${pad(date.getHours())}:${pad(date.getMinutes())}`;
}
