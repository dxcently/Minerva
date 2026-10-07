// Sending a note to a node over Aoide mail: the menu behind `[ send to node ]`, and the tab's 'sent' rows. Today only the seam; slice N2b
// fills this file. The perch does the sending (POST /hub/notes/<n>/send reads the note from the file); the page only asks.
//
// What pane.js and tab.js hold this file to:
//   sendOff(note)       '' when the note can be sent now, else the reason `[ send to node ]` is greyed (it is the button's title)
//   sendNote(note, at)  opens the send menu at `at` (a core/menu.js anchor: under(el)). note = { name, title, size }: the file name, the first
//                       line, and the bytes the pane or the listing counted. It is called only after store.js `settled(name)`, so the file on
//                       the perch is what is in the box; the perch is still the authority on the text it sends
//   Sent                the tab's 'sent' slot: a Preact component taking no props, null when nothing was sent
export const sendOff = () => 'sending a note needs Aoide mail (slice N2b)';

export function sendNote() {}

export const Sent = () => null;
