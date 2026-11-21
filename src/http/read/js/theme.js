// The reader's colour theme: the system's (no data-theme on <html>), or a
// choice of light or dark from the Account screen, kept in localStorage.
// A classic script loaded in <head> after the theme-color metas, so the
// stored choice applies before the first paint, and the system bars follow
// it. The colours are read.css's --bg.
(() => {
    const KEY = 'okf.read.theme';
    const BARS = Object.freeze({ light: '#ffffff', dark: '#191919' });
    const CHOICES = Object.freeze(['system', 'light', 'dark']);

    function stored() {
        try { return localStorage.getItem(KEY); } catch { return null; }
    }

    // The choice in force on this page: read once from storage, then set by
    // every choose(), so current() agrees with the applied theme even when
    // storage refuses the write.
    let active = (value => (value === 'light' || value === 'dark' ? value : 'system'))(stored());

    // Sets data-theme and the theme-color metas for `choice` (anything but
    // 'light' or 'dark' means the system's).
    function apply(choice) {
        const root = document.documentElement;
        const fixed = choice === 'light' || choice === 'dark';
        if (fixed) root.dataset.theme = choice;
        else delete root.dataset.theme;
        for (const meta of document.querySelectorAll('meta[name="theme-color"]')) {
            const scheme = meta.media.includes('dark') ? 'dark' : 'light';
            meta.content = BARS[fixed ? choice : scheme];
        }
    }

    self.READ_THEME = Object.freeze({
        CHOICES,
        // 'system', 'light' or 'dark'.
        current() { return active; },
        choose(choice) {
            active = choice === 'light' || choice === 'dark' ? choice : 'system';
            try {
                if (choice === 'light' || choice === 'dark') localStorage.setItem(KEY, choice);
                else localStorage.removeItem(KEY);
            } catch { /* storage blocked: the choice lasts for this visit only */ }
            apply(choice);
        },
    });
    apply(active);
})();
