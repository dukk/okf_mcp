// "On this page" (reader polish design, "Concept"): every heading of the
// concept's outline except the page title, whatever the levels. The API's
// title is the frontmatter's (src/okf/snapshot.rs), not a heading, so the
// first heading counts as the title only when it is level 1 and reads as
// that title (ignoring case and runs of spaces). A concept written as
// "# Title" then "## Parts" lists its parts; one made only of "#" sections,
// or with no title at all, lists every heading. No DOM, so Node tests it.

// `text` compared loosely: trimmed, spaces collapsed, lowercased.
function loose(text) {
    return String(text).trim().split(/\s+/).join(' ').toLowerCase();
}

// The outline entry that is the page title (`title`: the frontmatter
// title, or null), or null. The concept screen drops this heading from the
// outline and from the body; any other is kept.
export function titleHeading(headings, title) {
    const first = headings?.[0];
    if (!first || first.level !== 1 || !title) return null;
    return loose(first.heading) === loose(title) ? first : null;
}

// The headings to list, each with `depth` (0 for the shallowest listed).
export function sections(headings, title) {
    const list = headings ?? [];
    const shown = titleHeading(list, title) ? list.slice(1) : list;
    const top = Math.min(...shown.map(heading => heading.level));
    return shown.map(heading => ({ ...heading, depth: heading.level - top }));
}
