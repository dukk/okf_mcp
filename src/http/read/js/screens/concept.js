// A concept: breadcrumbs, title, outline, body (or, when the server
// shortened it, a button for the whole body) and what links to it.
import { h, fill } from '/ui/js/core/dom.js';
import { read, enc, encPath, KIND, ReadError } from '../api.js';
import { hrefs, hrefFor } from '../route.js';
import { crumbs, renderBody, message } from '../parts.js';

// The links read, or null when offline with no copy: the concept still shows.
async function backlinks(base, path) {
    try {
        return await read(`${base}/links/${encPath(path)}`);
    } catch (error) {
        if (error.kind === KIND.UNAVAILABLE) return null;
        throw error;
    }
}

function outline(catalog, path, headings) {
    const below = (headings ?? []).filter(heading => heading.level >= 2);
    if (!below.length) return null;
    return h('details', { class: 'outline', id: 'outline' }, h('summary', {}, 'Outline'),
        h('ul', {}, below.map(heading => h('li', { class: `level-${heading.level}` },
            h('a', { href: hrefs.concept(catalog, path, heading.slug) }, heading.heading)))));
}

function scrollTo(section) {
    if (!section) return;
    requestAnimationFrame(() => document.querySelector(`[data-slug="${CSS.escape(section)}"]`)?.scrollIntoView({ block: 'start' }));
}

export async function render(view, ctx) {
    const { catalog, path, section } = ctx.route;
    const base = `/catalogs/${enc(catalog)}`;
    const conceptUrl = `${base}/concepts/${encPath(path)}`;
    // A section route asks for the whole body, as the console does: over
    // concept_outline_chars the short answer has no body, so the heading to
    // scroll to would be missing (every navigation renders afresh, an
    // outline tap after "Show the whole concept" included).
    const firstUrl = section ? `${conceptUrl}?full=true` : conceptUrl;
    // Each read's refusal is acted on the moment it comes (ctx.settle: a
    // 401 wipes and signs in without waiting for the other read); what to
    // draw is decided once both have settled.
    const settled = await Promise.allSettled([ctx.settle(read(firstUrl)), ctx.settle(backlinks(base, path))]);
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
    const body = h('div', { id: 'body-slot' });
    // fill, not append: the outline is null for a concept with no sub-headings.
    fill(view,
        crumbs(catalog, path, false),
        h('h1', { id: 'concept-title' }, concept.title ?? path),
        outline(catalog, path, concept.outline),
        body);
    if (concept.body_html) {
        body.append(renderBody(concept.body_html));
        scrollTo(section);
    } else {
        const button = h('button', { type: 'button', id: 'show-full' }, 'Show the whole concept');
        button.addEventListener('click', async () => {
            button.disabled = true;
            try {
                const full = await read(`${conceptUrl}?full=true`);
                ctx.freshness(full);
                body.replaceChildren(renderBody(full.data.body_html));
                scrollTo(section);
            } catch (error) {
                if (!(error instanceof ReadError)) throw error;
                button.disabled = false;
                if (error.kind === KIND.UNAVAILABLE) {
                    body.append(message('The whole concept is not available offline.', null, 'full-failed'));
                    return;
                }
                // A 401 or a lost grant here must wipe or evict like any other read.
                await ctx.fail(error);
            }
        });
        body.append(message('This concept is long; only its outline was loaded.', button, 'shortened'));
    }
    view.append(h('h2', {}, 'Linked from'));
    if (!links) {
        view.append(h('p', { class: 'hint', id: 'backlinks' }, 'Links are not available offline.'));
    } else if (!links.data.backlinks.length) {
        view.append(h('p', { class: 'hint', id: 'backlinks' }, 'Nothing links here.'));
    } else {
        view.append(h('ul', { class: 'list', id: 'backlinks' }, links.data.backlinks.map(link =>
            h('li', {}, h('a', { href: hrefFor(catalog, link.path) }, link.title ?? link.path)))));
    }
}
