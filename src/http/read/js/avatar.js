// An actor's avatar: initials on a colour derived from a hash of the actor,
// so the same person keeps a colour on every screen and device. The colours
// are the classes avatar-0 to avatar-7, which read.css gets in a later
// task (the CSP allows no inline styles). No DOM, so Node tests it.

export const AVATAR_COLOURS = 8;

// The first letters of the first two words of `name` ('Alex Kim' -> 'AK'),
// or its first two letters when it is one word; '?' when it has none.
export function initials(name) {
    // \p{L}\p{N}: letters and digits in any script; everything else separates words.
    const words = String(name ?? '').split(/[^\p{L}\p{N}]+/u).filter(Boolean);
    if (!words.length) return '?';
    // [...word] splits by code point, so a letter outside the BMP stays whole.
    const letters = words.length === 1 ? [...words[0]].slice(0, 2) : [[...words[0]][0], [...words[1]][0]];
    return letters.join('').toUpperCase();
}

// The colour class of `actor` ('avatar-3'): FNV-1a over its UTF-16 code
// units, small and the same in every browser.
export function avatarClass(actor) {
    const text = String(actor ?? '');
    let hash = 0x811c9dc5;
    for (let at = 0; at < text.length; at += 1) {
        hash ^= text.charCodeAt(at);
        hash = Math.imul(hash, 0x01000193);
    }
    // >>> 0 reads the 32 bits as unsigned before the remainder.
    return `avatar-${(hash >>> 0) % AVATAR_COLOURS}`;
}
