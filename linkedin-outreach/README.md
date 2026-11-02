# linkedin-outreach
Reusable LinkedIn messaging automation over the chromebook-vm bridge.
Built 2026-11-02 for a 49-recipient recruiter outreach run (28 sent,
21 skipped on thread history, 0 failed, no LinkedIn warnings).

- `msg_cycle.py "Full Name"` — one recipient cycle: opens compose in the
  Messaging inbox, types the name, selects the 1st-connection match,
  reads the full thread. Outputs JSON {name, selected, thread, error}.
  Never touches the profile-page Message button.
- `msg_send.py "<message>"` — fills the open compose box with the exact
  draft text, wakes the Send button, clicks it, verifies the message
  landed in-thread. Outputs JSON {sent, error}.

The non-obvious piece is the Send-button wake sequence in `msg_send.py`:
LinkedIn keeps Send disabled on programmatic fills. The working order is
execCommand paragraph fill, then a keyboard-event burst
(keydown/keypress/keyup + InputEvent) to convince the composer the text
is user-typed, then verify `disabled:false` before clicking, then confirm
the sent bubble in the thread. Every send is verified in-thread before
it counts.

Operating rules (standing): read the full thread before any send, never
fire into a thread where they replied (especially a decline), minimum
1 minute between sends, full stop on any LinkedIn warning or captcha.
Contact lists and message drafts are never committed here.
