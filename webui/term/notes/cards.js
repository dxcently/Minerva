// Received cards: notes other nodes sent over Aoide mail, drawn in the notes tab. Today only the seam; slice N2b fills this file.
//
// What the rest of the folder holds this file to (index.js and tab.js import it, nothing else does):
//   badge      a signal: how many cards are unread. index.js gives it to the ✎ rail icon (core/registry.js tab badge); 0 draws nothing
//   Received   the tab's 'received (n)' slot: a Preact component taking no props, null when there is nothing to show
// Filling it also means registering the placed card above a chat's composer (composerExtras.add, core/registry.js), a toast when a card
// arrives, and the polling that feeds all three; this file starts that itself, so the badge is live while the tab is closed.
import { signal } from '../core/ui.js';

export const badge = signal(0);

export const Received = () => null;
