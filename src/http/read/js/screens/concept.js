// A concept: breadcrumbs, its title, a meta row (type, up to three tags, the
// last edit, reading time), its description as a lede, "On this page", the
// body in the serif reading type, and what links to it, one row per
// document. The reader always reads the whole body (?full=true): the
// server's outline limit is meant for agents.
import { h, fill } from '/ui/js/core/dom.js';
import { ago } from '/ui/js/core/format.js';
import { read, enc, encPath, KIND, ReadError } from '../api.js';
import { hrefs, hrefFor, parentOf } from '../route.js';
import { crumbs, lede, pageTitle, pill, renderBody, row, sectionHead } from '../parts.js';
import { icon } from '../icons.js';
import { sections, titleHeading } from '../outline.js';
import { backlinkNote, dedupeBacklinks } from '../backlinks.js';
import { fileName, readingMinutes } from '../text.js';

const MAX_TAGS = 3;

// The links read, or null when offline with no copy: the concept still shows.
async function backlinks(base, path) {
    try {
        return await read(`${base}/links/${encPath(path)}`);
    } catch (error) {
        if (error.kind === KIND.UNAVAILABLE) return null;
        throw error;
    }
}

// The newest revision ({ actor, actor_label, at }), or null. It is
// optional: the concept never waits for it (render fills the meta row when
// it comes), and the row does without it offline (the worker keeps no
// history) or when the history is refused. A refusal that is true of every
// read (a session that ended, a lost catalog) still acts through ctx.fail:
// it wipes or evicts as any read's would. It never rejects with a ReadError.
async function lastEdit(ctx, base, path) {
    try {
        const result = await read(`${base}/history/${encPath(path)}?limit=1`);
        return result.data.revisions?.[0] ?? null;
    } catch (error) {
        if (!(error instanceof ReadError)) throw error;
        if (error.kind === KIND.SIGNED_OUT || error.kind === KIND.CATALOG_GONE) await ctx.fail(error);
        return null;
    }
}

// The meta row: the type and tags as pills, the reading time, then `edited`
// (the last edit's slot, hidden until the history answers).
function metaRow(concept, edited) {
    const fields = concept.frontmatter ?? {};
    const tags = Array.isArray(fields.tags) ? fields.tags.filter(tag => typeof tag === 'string').slice(0, MAX_TAGS) : [];
    return h('div', { class: 'meta', id: 'concept-meta' },
        typeof fields.type === 'string' && fields.type ? pill(fields.type, 'type') : null,
        tags.map(tag => pill(tag)),
        h('span', { class: 'fact' }, `${readingMinutes(concept.chars)} min read`),
        edited);
}

// "On this page": every heading but the page title, linking to its section.
function onThisPage(catalog, path, listed) {
    if (!listed.length) return null;
    return h('details', { class: 'toc', id: 'outline' },
        h('summary', {}, icon('list'), `On this page · ${listed.length} ${listed.length === 1 ? 'section' : 'sections'}`, icon('down', 'icon-end')),
        h('ol', {}, listed.map(heading => h('li', { class: `depth-${Math.min(heading.depth, 3)}` },
            h('a', { href: hrefs.concept(catalog, path, heading.slug) }, heading.heading)))));
}

// "Linked from": one row per linking document, with its count.
function linkedFrom(catalog, links, rootLabel) {
    if (!links) return [sectionHead('Linked from'), h('p', { class: 'hint', id: 'backlinks' }, 'Links are not available offline.')];
    const rows = dedupeBacklinks(links.data.backlinks);
    if (!rows.length) return [sectionHead('Linked from'), h('p', { class: 'hint', id: 'backlinks' }, 'Nothing links here.')];
    return [sectionHead('Linked from', { count: rows.length }), h('div', { class: 'rows', id: 'backlinks' }, rows.map(link => {
        const name = fileName(link.path);
        const folder = parentOf(link.path).split('/').pop() || rootLabel;
        const title = name === 'log.md' ? 'Log' : link.title ?? (name === 'index.md' ? folder : name);
        const glyph = name === 'log.md' ? 'log' : name === 'index.md' ? 'folder' : 'doc';
        const note = backlinkNote(link);
        return row({ href: hrefFor(catalog, link.path), glyph, title, end: note ? h('span', { class: 'r' }, note) : null });
    }))];
}

function scrollTo(section) {
    if (!section) return;
    requestAnimationFrame(() => document.querySelector(`[data-slug="${CSS.escape(section)}"]`)?.scrollIntoView({ block: 'start' }));
}

export async function render(view, ctx) {
    const { catalog, path, section } = ctx.route;
    const base = `/catalogs/${enc(catalog)}`;
    // The last edit is read beside the others but never awaited with them:
    // the concept draws as soon as its own reads settle.
    const edit = lastEdit(ctx, base, path);
    // Each read's refusal is acted on the moment it comes (ctx.settle: a
    // 401 wipes and signs in without waiting for the other read); what to
    // draw is decided once both have settled.
    const settled = await Promise.allSettled([
        ctx.settle(read(`${base}/concepts/${encPath(path)}?full=true`)),
        ctx.settle(backlinks(base, path)),
    ]);
    const refused = settled.filter(outcome => outcome.status === 'rejected').map(outcome => outcome.reason);
    const unexpected = refused.find(reason => !(reason instanceof ReadError));
    if (unexpected) throw unexpected;
    if (refused.length) {
        ctx.drawWorst(refused);
        return;
    }
    const [result, links] = settled.map(outcome => outcome.value);
    if (!ctx.live()) return;
    ctx.freshness(result);
    ctx.freshness(links);
    const concept = result.data;
    const headings = concept.outline ?? [];
    const article = renderBody(concept.body_html ?? '', { serif: true });
    // The page title is drawn above: its heading in the body would repeat
    // it. Only a level-1 first heading that reads as the frontmatter title
    // goes; an untitled concept keeps its whole body.
    const top = titleHeading(headings, concept.title);
    if (top) article.querySelector(`h1[data-slug="${CSS.escape(top.slug)}"]`)?.remove();
    const description = typeof concept.frontmatter?.description === 'string' ? concept.frontmatter.description : null;
    const edited = h('span', { class: 'fact', id: 'concept-edited', hidden: true });
    // fill, not append: the lede and the outline may be null.
    fill(view,
        crumbs(catalog, path, false, { root: ctx.label(), titles: ctx.titles() }),
        pageTitle(concept.title ?? fileName(path), 'concept-title'),
        metaRow(concept, edited),
        lede(description, 'concept-lede'),
        onThisPage(catalog, path, sections(headings, concept.title)),
        article,
        linkedFrom(catalog, links, ctx.label()));
    scrollTo(section);
    // The edit line, when the history answers, on this screen only.
    edit.then(found => {
        if (!found || !ctx.live()) return;
        edited.textContent = `Edited ${ago(found.at)} by ${found.actor_label ?? found.actor}`;
        edited.hidden = false;
    });
}
