// The one import point for the view layer: Preact, its hooks, signals and
// htm, all vendored under vendor/ (see vendor/update.sh; no build, no npm).
//   html`<div class="row">${text}</div>`   text is always text (P5)
// Never use dangerouslySetInnerHTML, innerHTML or eval anywhere in the page.
import { h, render, Fragment, Component } from '../vendor/preact.js';
import { useRef, useLayoutEffect } from '../vendor/hooks.js';
import htm from '../vendor/htm.js';

export { h, render, Fragment, Component };
export { useEffect, useLayoutEffect, useRef, useState, useMemo } from '../vendor/hooks.js';
export { signal, computed, effect, batch, untracked } from '../vendor/signals.js';

export const html = htm.bind(h);

// Why a control is greyed until the Minerva hub exists.
export const HUB = 'needs the hub';

// Clickable text: looks like text, acts like a control, announces itself as
// one (web-ui.md 2.3). There are no <button> elements anywhere in the page.
// `off` draws the same words faint and inert.
// Keys: Enter or Space activates, never a held key (e.repeat). Inside an ask
// block a K acts on no key at all: the event bubbles to the ask's own
// handler, the only thing that answers an ask by key (panes/session.js).
export function K({ on, cls = '', title, children, off }) {
  if (off) return html`<span class=${'dis ' + cls} aria-disabled="true" title=${title}>${children}</span>`;
  const act = (e) => { e.stopPropagation(); e.preventDefault(); on(e); };
  const key = (e) => {
    if (e.repeat || (e.key !== 'Enter' && e.key !== ' ')) return;
    if (e.currentTarget.closest('.ask')) return;
    act(e);
  };
  return html`<span class=${'k ' + cls} role="button" tabindex="0" title=${title}
    onClick=${act} onKeyDown=${key}>${children}</span>`;
}

// A renderer may hand back a DOM Node (built with textContent only) or a
// Preact vnode (web-ui.md section 10). This mounts either.
export function Rendered({ value }) {
  const ref = useRef(null);
  useLayoutEffect(() => {
    if (ref.current && value instanceof Node) ref.current.replaceChildren(value);
  }, [value]);
  if (value instanceof Node) return html`<div class="mounted" ref=${ref}></div>`;
  return value ?? null;
}
