## Added
- The Account page marks its view `data-loaded="true"` once all six of its reads (profile, ceiling, passkeys, linked accounts, sessions, recent sign-ins) have settled, failed reads included. It is a signal for browser journeys, not for people.
- `Ui::wait_checked` in the browser harness: waits until a checkbox is ticked.
- Journey `account::the_account_page_marks_itself_loaded_after_its_slowest_section`: holds the session list read and checks the marker is absent until it answers.

## Changed
- `account::a_member_links_an_identity_and_keeps_their_other_sessions` (issue #373) waits for the page's loaded marker before it clicks, and again after the provider sends the browser back; checks the phone's row before linking (so a missing row afterwards can only be the link's doing); and waits for "Keep me signed in elsewhere" to be ticked before it starts the link. Before, it clicked the box and the Link button as soon as the button appeared, while the other five sections were still arriving and moving the controls.

## Notes
- The session row was never created late: the journey makes the phone's session before the page's first load. The issue's other suggestion (refresh the list until the row appears) would hide a lost "keep" choice, so the journey does not do it.
- The page's own session list cannot lose a newer answer on this journey (one read at load). Its other callers (`drawSessions` after a password change, a passkey added, a session signed out) can overlap and are not numbered the way `lessons.md` asks; that is a separate problem and was left alone here.
- Not reproduced one-for-one: the exact failure was seen once in a full gate run. Under a synthetic load of eight concurrent copies of the Account journeys, the same family of timeouts showed on other journeys (#383, #431 and the account deletion journey), which also click as soon as one control appears; they were not changed here.
