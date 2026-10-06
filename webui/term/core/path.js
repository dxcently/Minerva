// Where a tool-input path points, worked out as text only (no filesystem here).
// A tool line's path comes from the model, so the page offers it to the launcher
// only when it lands inside the session's folder: `resolveIn(cwd, raw)` returns
// { path } (absolute, `.` and `..` folded, no trailing slash) or { why } (the reason
// it is not offered). A symlink inside the folder is not seen from here.
const BAD = /[\u0000-\u001f\u007f-\u009f\u202a-\u202e\u2066-\u2069]/;

function fold(p) {
  const out = [];
  for (const seg of p.split('/')) {
    if (seg === '' || seg === '.') continue;
    if (seg === '..') out.pop(); else out.push(seg);
  }
  return '/' + out.join('/');
}

export function resolveIn(cwd, raw) {
  if (typeof raw !== 'string' || !raw) return { why: 'no path' };
  if (BAD.test(raw)) return { why: 'the path has control or direction characters' };
  if (typeof cwd !== 'string' || !cwd.startsWith('/')) return { why: 'the session folder is not known yet' };
  if (raw === '~' || raw.startsWith('~/')) return { why: 'a ~ path (it would be expanded by the launcher)' };
  const base = fold(cwd);
  const path = fold(raw.startsWith('/') ? raw : base + '/' + raw);
  const inside = base === '/' || path === base || path.startsWith(base + '/');
  return inside ? { path } : { why: `outside the session folder (${base})` };
}

// A folder as a person pastes it, as the WSL path the server wants: `C:\Users\x\proj`
// or `C:/Users/x/proj` becomes `/mnt/c/Users/x/proj`; `\\wsl.localhost\<distro>\home\x`
// (and `\\wsl$\...`) becomes `/home/x`. Anything else comes back unchanged (trimmed, and
// without surrounding quotes, which Explorer's "copy as path" adds).
export function toWslPath(raw) {
  const p = String(raw || '').trim().replace(/^"(.*)"$/, '$1');
  const slashes = (s) => s.replace(/\\/g, '/').replace(/\/+$/, '');
  const drive = /^([A-Za-z]):[\\/]*(.*)$/.exec(p);
  if (drive) return '/mnt/' + drive[1].toLowerCase() + '/' + slashes(drive[2]);
  const unc = /^\\\\(?:wsl\.localhost|wsl\$)\\[^\\]+(?:\\(.*))?$/i.exec(p);
  if (unc) return '/' + slashes(unc[1] || '');
  return p;
}
