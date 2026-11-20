// The console's "Send feedback" form (issue #793), in the drawer. Shown only
// to someone signed in with an account on a server with `[feedback]`
// (shell.js decides, from /ui/brand.json's `feedback_form`). It posts to
// POST /api/v1/feedback, which checks and queues it as the send_feedback
// tool does; the server's disclosure text is shown as given.
import { h } from '../core/dom.js';
import { api } from '../core/api.js';
import { drawerTicket } from '../core/drawer.js';
import { guard, once } from '../core/pending.js';
import { toast } from '../core/toast.js';

// The one feedback write: a reopened drawer draws its Send button disabled
// while an earlier send is still running.
const SEND = 'feedback:send';

// Opens the form. `form` is brand.json's `feedback_form`
// ({disclosure, max_message_chars, kinds}).
export function openFeedback(form) {
    const ticket = drawerTicket(document.getElementById('main'));
    const message = h('textarea', {
        id: 'feedback-message', name: 'message', rows: 6, required: true,
        maxlength: form.max_message_chars,
    });
    const kind = h('select', { id: 'feedback-kind', name: 'kind' },
        form.kinds.map(name => h('option', { value: name, selected: name === 'other' }, name)));
    const rating = h('select', { id: 'feedback-rating', name: 'rating' },
        h('option', { value: '' }, 'No rating'),
        [1, 2, 3, 4, 5].map(n => h('option', { value: String(n) }, String(n))));
    const status = h('p', { id: 'feedback-status', role: 'status', hidden: true });
    const send = guard(h('button', { type: 'submit', id: 'feedback-send' }, 'Send feedback'), SEND);
    const show = (text, kind) => {
        status.textContent = text;
        status.className = kind === 'error' ? 'error' : 'muted';
        status.dataset.state = kind;
        status.hidden = false;
    };
    const submit = event => {
        event.preventDefault();
        status.hidden = true;
        const text = message.value.trim();
        if (!text) { show('Write your feedback first.', 'error'); return; }
        const body = { message: text, kind: kind.value };
        if (rating.value) body.rating = Number(rating.value);
        once(SEND, async () => {
            try {
                await api.post('/feedback', body, { quiet: true });
            } catch (error) {
                // Kept as typed, so the person can fix it or try later. A
                // drawer closed or reopened meanwhile has no place for the
                // reason, so it goes to a toast instead.
                const reason = `Not sent: ${error.message}`;
                if (ticket.current()) show(reason, 'error'); else toast(reason, 'error');
                return;
            }
            if (!ticket.current()) { toast('Thank you. Your feedback was sent.', 'ok'); return; }
            message.value = '';
            rating.value = '';
            show('Thank you. Your feedback was sent.', 'ok');
        });
    };
    ticket.open('Send feedback', null,
        h('form', { id: 'feedback-form', onsubmit: submit },
            h('p', { class: 'muted small', id: 'feedback-disclosure' }, form.disclosure),
            h('label', {}, 'Your feedback', message),
            h('label', {}, 'About', kind),
            h('label', {}, 'Rating (optional)', rating),
            send,
            status));
}
