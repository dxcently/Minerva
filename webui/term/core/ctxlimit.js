// The context limit of a model, from the note on its row in `GET /hub/models`. eidolon writes that note (providers/src/catalog.rs,
// `describe`) as `provider · 10k ctx · $0.3/1.2 per M · reasoning`: the context in thousands, rounded down, only when the catalog knows
// it; the perch passes the last column through as it is (perch/src/models.rs). A note with no `ctx` word names no limit, and the
// status line then shows `-`: nothing here guesses one. Pure: no fetch, no DOM.
//
//   ctxLimit(note)     tokens, or null when the note names none
//   limitTable(rows)   Map of key -> tokens | null, for the rows `GET /hub/models` sent ([] or anything else: an empty Map)

// `10k ctx`, `1M ctx`, `1.5M ctx`, `8192 ctx`: a number, an optional k or M, then the word. Not inside a longer word or number.
const CTX = /(?:^|[^\w.])(\d+(?:\.\d+)?)\s*([km]?)\s*ctx\b/i;
const UNIT = { '': 1, k: 1000, m: 1000000 };

export function ctxLimit(note) {
  const m = CTX.exec(typeof note === 'string' ? note : '');
  if (!m) return null;
  const n = Math.round(Number(m[1]) * UNIT[m[2].toLowerCase()]);
  return n > 0 ? n : null; // `0k ctx` is what eidolon prints for a context under a thousand: no usable limit
}

export function limitTable(rows) {
  const t = new Map();
  if (!Array.isArray(rows)) return t;
  for (const r of rows) if (r && typeof r.key === 'string') t.set(r.key, ctxLimit(r.note));
  return t;
}
