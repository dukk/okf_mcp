// What a refused read does (reader design, "Error handling"), apart from
// the DOM: main.js builds one handler per screen it shows, and gives it the
// actions that touch the session, the caches, the remembered catalog, the
// hash and the view. Kept free of the DOM so tests/read_worker/ can run it
// in Node, with deferred reads.
import { KIND, ReadError } from './api.js';
import { hrefs } from './route.js';

// The kinds from most to least serious: when several reads of one screen
// are refused, the most serious one alone draws.
export const SEVERITY = Object.freeze([
    KIND.SIGNED_OUT, KIND.CATALOG_GONE, KIND.CONCEPT_GONE, KIND.LOCKED, KIND.FAILED, KIND.UNAVAILABLE,
]);

// A kind's place in SEVERITY; one it does not list ranks last.
function rank(error) {
    const at = SEVERITY.indexOf(error?.kind);
    return at < 0 ? SEVERITY.length : at;
}

// The most serious of `errors` (the first of equals).
export function worst(errors) {
    return [...errors].sort((a, b) => rank(a) - rank(b))[0];
}

// The refusal handler of one shown screen.
//
// `route` is the screen's route with its catalog fixed when it was shown,
// so a refusal that answers after the user moved on acts on what was asked
// about. `live()` says whether that screen still shows. `actions`:
//   signIn()                   wipe, then show the signed-out screen
//   evictCatalog(catalog)      delete a catalog's cached reads
//   evictDocument(catalog, kind, path)  delete one document's copies
//   current()                  the remembered catalog now
//   choose(catalog)            remember a catalog (null forgets it)
//   go(hash)                   navigate to a reader route
//   paint(error)               draw the refusal's notice on the screen
export function refusals(route, live, actions) {
    // What a refusal changes, whichever screen shows now: a 401 or a lost
    // grant is true even when the screen that asked has been left. Run as
    // soon as that one request is refused, never after its siblings.
    async function effects(error) {
        switch (error.kind) {
            case KIND.SIGNED_OUT:
                await actions.signIn();
                return;
            case KIND.CATALOG_GONE:
                if (route.catalog) await actions.evictCatalog(route.catalog);
                // Forget it only while it is still the remembered one: a
                // late refusal never undoes a newer choice.
                if (route.catalog === actions.current()) actions.choose(null);
                return;
            case KIND.CONCEPT_GONE:
                await actions.evictDocument(route.catalog, route.screen === 'document' ? 'documents' : 'concepts', route.path);
                return;
            case KIND.LOCKED:
            case KIND.UNAVAILABLE:
            case KIND.FAILED:
                return;
        }
    }

    // What a refusal shows, on the screen that asked, while it is still shown.
    function draw(error) {
        if (!live()) return;
        switch (error.kind) {
            case KIND.SIGNED_OUT:
                return; // effects() has already left for sign-in.
            case KIND.CATALOG_GONE:
                actions.go(hrefs.catalogs(route.catalog));
                return;
            case KIND.CONCEPT_GONE:
            case KIND.LOCKED:
            case KIND.UNAVAILABLE:
            case KIND.FAILED:
                actions.paint(error);
                return;
        }
    }

    // A single refused read (a screen's, or a button's later): its effects,
    // then its drawing.
    async function fail(error) {
        await effects(error);
        draw(error);
    }

    return {
        effects,
        draw,
        fail,
        // For a screen that makes several reads at once: `settle` runs each
        // read's refusal effects the moment it is refused (marking the error
        // handled), and `drawWorst` draws once all have settled.
        settle: promise => promise.catch(async error => {
            if (error instanceof ReadError) {
                error.handled = true;
                await effects(error);
            }
            throw error;
        }),
        drawWorst: errors => draw(worst(errors)),
        // What a screen's render threw: a refusal already handled by
        // `settle` is drawn only; any other refusal fails; anything that is
        // not a refusal is a bug, and is thrown on.
        async caught(error) {
            if (!(error instanceof ReadError)) throw error;
            if (error.handled) draw(error);
            else await fail(error);
        },
    };
}
