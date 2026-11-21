// A fake of src/http/read/js/icons.js: an icon as a plain object naming it.
export function icon(name, cls = '') {
    return { tag: 'svg', attrs: { class: cls ? `icon ${cls}` : 'icon', 'data-icon': name }, children: [] };
}
