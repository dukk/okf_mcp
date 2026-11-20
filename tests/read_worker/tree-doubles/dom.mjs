// A fake of /ui/js/core/dom.js: elements are plain objects to inspect.
export function h(tag, attrs = {}, ...children) {
    return { tag, attrs: attrs ?? {}, children: children.flat(Infinity).filter(c => c !== null && c !== undefined && c !== false) };
}
