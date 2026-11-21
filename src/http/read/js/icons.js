// The reader's icons, from its sprite (/read/icons.svg, precached with the
// shell), as <svg class="icon"><use href="/read/icons.svg#i-NAME"></svg>.
// dom.js's h() makes HTML elements, and an <svg> drawn as HTML shows
// nothing, so icons are made here, in the SVG namespace.
const SVG = 'http://www.w3.org/2000/svg';
export const SPRITE = '/read/icons.svg';

// The icon `name` (a symbol id without its 'i-'), with extra classes `cls`.
export function icon(name, cls = '') {
    const svg = document.createElementNS(SVG, 'svg');
    svg.setAttribute('class', cls ? `icon ${cls}` : 'icon');
    svg.setAttribute('aria-hidden', 'true');
    svg.setAttribute('focusable', 'false');
    const use = document.createElementNS(SVG, 'use');
    use.setAttribute('href', `${SPRITE}#i-${name}`);
    svg.append(use);
    return svg;
}
