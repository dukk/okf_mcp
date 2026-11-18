// Numbers a page's overlapping reads so only the newest one draws.
//
// Reads of one list overlap (a drawer action's reload while another's is in
// flight, a Refresh) and can finish out of order; an older answer must never
// draw over a newer one, and an answer that arrives after the page was left
// must not draw into whatever page is shown now.
//
// `const reads = latest(main)` gives one counter per list. Call `reads()`
// before the read starts (before any await): it takes the next number and
// returns `current`, a function that is true only while no newer read has
// started and `view` (the page's container) is still in the document. Check
// it after the await and draw nothing when it is false.
export function latest(view) {
    let newest = 0;
    return () => {
        const mine = ++newest;
        return () => mine === newest && view.isConnected;
    };
}
