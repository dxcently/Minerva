// Server-sent events over fetch + ReadableStream, so the token rides an
// Authorization header instead of `?token=` (web-ui.md 5.2). The door writes
// one `data:` line of JSON per frame, no `id:`, no `event:` (stream.rs:44-46).
//
// Resolves when the stream ends: { status } (200 = it opened and later
// ended), or { status, data } for a refusal at connect (data: the JSON body,
// at most 4 KiB read, else null). Rejects on a network error / abort; never
// on an HTTP status.
export async function stream(url, headers, onFrame, signal) {
  const r = await fetch(url, { headers, signal, cache: 'no-store' });
  if (!r.ok || !r.body) return { status: r.status, data: await refusal(r) };
  const reader = r.body.getReader();
  const dec = new TextDecoder();
  let buf = '', cr = false;
  for (;;) {
    const { value, done } = await reader.read();
    // Normalise CRLF / CR to LF across chunk boundaries: a chunk ending in
    // `\r` holds that `\r` back, so a `\r\n` split over two chunks is one
    // line break, not two (which would end a frame early). At the end of
    // the stream a held `\r` is a line break of its own.
    let t = (cr ? '\r' : '') + (done ? dec.decode() : dec.decode(value, { stream: true }));
    cr = !done && t.endsWith('\r');
    if (cr) t = t.slice(0, -1);
    buf += t.replace(/\r\n?/g, '\n');
    let i;
    while ((i = buf.indexOf('\n\n')) >= 0) {
      const block = buf.slice(0, i);
      buf = buf.slice(i + 2);
      const data = block.split('\n')
        .filter((l) => l.startsWith('data:'))
        .map((l) => l.slice(5).replace(/^ /, ''))
        .join('\n');
      if (!data) continue;
      let f;
      try { f = JSON.parse(data); } catch { continue; }
      if (f && typeof f.type === 'string') onFrame(f); // P6: no type, no frame
    }
    if (done) break;
  }
  return { status: r.status };
}

// A refusal's body: at most 4 KiB read, parsed as JSON, else null.
async function refusal(r) {
  try {
    if (!r.body) return null;
    const reader = r.body.getReader();
    const dec = new TextDecoder();
    let t = '', n = 0;
    while (n < 4096) {
      const { value, done } = await reader.read();
      if (done) break;
      n += value.length;
      t += dec.decode(value, { stream: true });
    }
    reader.cancel().catch(() => {});
    return JSON.parse(t.slice(0, 4096));
  } catch { return null; }
}
