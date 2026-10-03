// webui-cdp.mjs: drive the Minerva web UI in a throwaway headless Edge over the Chrome
// DevTools Protocol and report what the page drew. Plain Node (v22+), no dependencies.
// Transport: --remote-debugging-pipe (CDP on the child's fds 3/4, NUL-delimited). There is no
// DevTools TCP port, so no other local process can attach and read the token.
// Needs msedge.exe to stay the browser process: run it from a normal shell or WSL (from some
// sandboxed shells msedge.exe hands off to another process and the pipe closes: FAIL launch).
//
// usage (from WSL):
//   "/mnt/c/Program Files/nodejs/node.exe" bin/webui-cdp.mjs <page> [step ...]
//   <page>  the redirect file's Windows path from `bin/perch-open.sh --no-browser`, or an
//           http(s) URL that carries no token (e.g. the no-token screen).
// steps (one argv token each, name=value):
//   wait-text=<s>   poll document.body.innerText until it contains s
//   gone-text=<s>   poll until it no longer contains s
//   click-text=<s>  click the first visible `[role=button], .k, .opt, .atab` whose text includes s
//   rclick-text=<s> same, as a contextmenu event at its centre
//   key=<k>         key down/up (Enter, Escape, Tab, a letter, ":" ...) to document.activeElement
//   type=<s>        Input.insertText into the focused element
//   text=<css>      print `TEXT <css>: <innerText>` (first match; `body` for the whole body)
//   eval=<js>       print `EVAL: <JSON result>` (refused if it mentions location, sessionStorage,
//                   localStorage or token)
//   shot=<path.png> write a screenshot
//   sleep=<ms>
//   timeout=<s>     timeout for the following wait steps (default 20)
// env: MINERVA_EDGE overrides the msedge.exe path.
// Exit 0 when every step passed, 1 on the first failing step.
//
// Secret rules: the page URL carries #token= briefly. This tool never prints or records
// location, document.URL, any CDP event, any target url or any navigation URL, and never
// subscribes to Network.*.

import { spawn, spawnSync } from 'node:child_process';
import { existsSync, mkdtempSync, readFileSync, rmSync, writeFileSync, mkdirSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, dirname, resolve, basename } from 'node:path';
import { pathToFileURL } from 'node:url';

const EDGE = process.env.MINERVA_EDGE || 'C:\\Program Files (x86)\\Microsoft\\Edge\\Application\\msedge.exe';
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

const argv = process.argv.slice(2);
if (argv.length < 1 || argv[0] === '-h' || argv[0] === '--help') {
  console.error('usage: node webui-cdp.mjs <page> [name=value ...]  (see the header of this file)');
  process.exit(argv.length < 1 ? 2 : 0);
}
const page = argv[0];
const steps = argv.slice(1);

// A local redirect file (from bin/perch-open.sh) is read here and its target is navigated
// to directly, the way a typed URL is (Sec-Fetch-Site: none). Loading the file and letting
// its script redirect sends Sec-Fetch-Site: cross-site, which the perch's gate refuses with
// 403 (perch/src/gate.rs sec_fetch_ok). The target URL carries the token: it is held in
// memory only and never printed. MINERVA_CDP_VIA_FILE=1 navigates to the file itself instead.
function pageUrl(p) {
  if (/^https?:\/\//i.test(p)) return p;
  if (!process.env.MINERVA_CDP_VIA_FILE) {
    try {
      const m = /location\.replace\("(http:\/\/127\.0\.0\.1:\d+\/[^"]*)"\)/.exec(readFileSync(p, 'utf8'));
      if (m) { console.log('note: redirect file read by the tool, target navigated directly (not via the file)'); return m[1]; }
    } catch { /* fall through to the file URL */ }
  }
  return pathToFileURL(p).href;
}

// CDP over the browser's pipe: we write to the child's fd 3 and read its fd 4, messages
// are JSON separated by a NUL byte. No TCP port exists, so no other process can attach.
// `sessionId` (flat target session) is set after Target.attachToTarget; calls made before
// that go to the browser itself.
class Cdp {
  constructor(wr, rd) {
    this.wr = wr;
    this.id = 0;
    this.sessionId = null;
    this.pending = new Map();
    this.buf = '';
    this.closed = false;
    rd.setEncoding('utf8');
    rd.on('data', (chunk) => {
      this.buf += chunk;
      let i;
      while ((i = this.buf.indexOf('\0')) >= 0) {
        const raw = this.buf.slice(0, i);
        this.buf = this.buf.slice(i + 1);
        let m;
        try { m = JSON.parse(raw); } catch { continue; }
        // Events (no id) are dropped unread; only responses are kept.
        if (m.id && this.pending.has(m.id)) {
          const { res, rej } = this.pending.get(m.id);
          this.pending.delete(m.id);
          // Only the error message is kept; never the params or any event.
          if (m.error) rej(new Error(String(m.error.message || 'cdp error')));
          else res(m.result);
        }
      }
    });
    const gone = () => {
      this.closed = true;
      for (const { rej } of this.pending.values()) rej(new Error('devtools pipe closed'));
      this.pending.clear();
    };
    rd.on('close', gone);
    rd.on('error', gone);
    wr.on('error', gone);
  }
  send(method, params = {}) {
    const id = ++this.id;
    return new Promise((res, rej) => {
      if (this.closed) return rej(new Error('devtools pipe closed'));
      this.pending.set(id, { res, rej });
      const msg = { id, method, params };
      if (this.sessionId && !method.startsWith('Browser.') && !method.startsWith('Target.')) msg.sessionId = this.sessionId;
      this.wr.write(JSON.stringify(msg) + '\0');
    });
  }
  async evalJs(expr) {
    const r = await this.send('Runtime.evaluate', { expression: expr, returnByValue: true, awaitPromise: true });
    if (r.exceptionDetails) {
      throw new Error('page exception: ' + String(r.exceptionDetails.exception?.description || r.exceptionDetails.text || '').split('\n')[0]);
    }
    return r.result.value;
  }
}

const KEYS = {
  Enter: { code: 'Enter', vk: 13, text: '\r' },
  Escape: { code: 'Escape', vk: 27 },
  Tab: { code: 'Tab', vk: 9 },
  Backspace: { code: 'Backspace', vk: 8 },
  Delete: { code: 'Delete', vk: 46 },
  ArrowUp: { code: 'ArrowUp', vk: 38 },
  ArrowDown: { code: 'ArrowDown', vk: 40 },
  ArrowLeft: { code: 'ArrowLeft', vk: 37 },
  ArrowRight: { code: 'ArrowRight', vk: 39 },
  Home: { code: 'Home', vk: 36 },
  End: { code: 'End', vk: 35 },
  ' ': { code: 'Space', vk: 32, text: ' ' },
};

async function pressKey(cdp, k) {
  let mods = 0;
  let key = k;
  const m = /^((?:ctrl|alt|shift|meta)\+)+/i.exec(k);
  if (m) {
    for (const part of m[0].toLowerCase().split('+')) {
      if (part === 'alt') mods |= 1;
      if (part === 'ctrl') mods |= 2;
      if (part === 'meta') mods |= 4;
      if (part === 'shift') mods |= 8;
    }
    key = k.slice(m[0].length);
  }
  const def = KEYS[key];
  let ev;
  if (def) {
    ev = { key, code: def.code, windowsVirtualKeyCode: def.vk, nativeVirtualKeyCode: def.vk, modifiers: mods };
    if (def.text && !(mods & 6)) ev.text = def.text;
  } else if ([...key].length === 1) {
    const up = key.toUpperCase();
    const isAlnum = /[A-Za-z0-9]/.test(key);
    ev = {
      key,
      code: isAlnum ? (/[0-9]/.test(key) ? 'Digit' + key : 'Key' + up) : '',
      windowsVirtualKeyCode: isAlnum ? up.charCodeAt(0) : key.charCodeAt(0),
      modifiers: mods,
    };
    if (!(mods & 6)) ev.text = key;
  } else {
    throw new Error(`unknown key "${k}"`);
  }
  await cdp.send('Input.dispatchKeyEvent', { type: ev.text ? 'keyDown' : 'rawKeyDown', ...ev });
  await cdp.send('Input.dispatchKeyEvent', { type: 'keyUp', ...ev, text: undefined });
}

const CLICKABLE = '[role=button], .k, .opt, .atab';
const findExpr = (s) => `(() => {
  const want = ${JSON.stringify(s)};
  for (const e of document.querySelectorAll(${JSON.stringify(CLICKABLE)})) {
    const r = e.getBoundingClientRect();
    const cs = getComputedStyle(e);
    if (r.width <= 0 || r.height <= 0 || cs.visibility === 'hidden' || cs.display === 'none') continue;
    if ((e.innerText || '').trim().includes(want)) {
      e.scrollIntoView({ block: 'center', inline: 'center' });
      const q = e.getBoundingClientRect();
      return { x: q.left + q.width / 2, y: q.top + q.height / 2 };
    }
  }
  return null;
})()`;

async function bodyText(cdp) {
  return String((await cdp.evalJs('document.body ? document.body.innerText : ""')) ?? '');
}

async function poll(cdp, timeoutS, want, present, label) {
  const end = Date.now() + timeoutS * 1000;
  let body = '';
  while (Date.now() < end) {
    try { body = await bodyText(cdp); } catch { body = ''; }
    if (body.includes(want) === present) return;
    await sleep(200);
  }
  const e = new Error(`${label} ${JSON.stringify(want)} not satisfied in ${timeoutS}s; body:\n${body.slice(0, 2000)}`);
  throw e;
}

async function runStep(cdp, step, state) {
  const i = step.indexOf('=');
  if (i < 1) throw new Error('step must be name=value');
  const name = step.slice(0, i);
  const val = step.slice(i + 1);
  switch (name) {
    case 'timeout': {
      const n = Number(val);
      if (!(n > 0)) throw new Error('timeout needs seconds > 0');
      state.timeout = n;
      return `ok ${step}`;
    }
    case 'sleep': await sleep(Number(val) || 0); return `ok ${step}`;
    case 'wait-text': await poll(cdp, state.timeout, val, true, 'wait-text'); return `ok ${step}`;
    case 'gone-text': await poll(cdp, state.timeout, val, false, 'gone-text'); return `ok ${step}`;
    case 'click-text':
    case 'rclick-text': {
      const pt = await cdp.evalJs(findExpr(val));
      if (!pt) throw new Error(`no visible clickable text matching ${JSON.stringify(val)}`);
      if (name === 'click-text') {
        await cdp.send('Input.dispatchMouseEvent', { type: 'mouseMoved', x: pt.x, y: pt.y });
        await cdp.send('Input.dispatchMouseEvent', { type: 'mousePressed', x: pt.x, y: pt.y, button: 'left', clickCount: 1 });
        await cdp.send('Input.dispatchMouseEvent', { type: 'mouseReleased', x: pt.x, y: pt.y, button: 'left', clickCount: 1 });
      } else {
        await cdp.evalJs(`(() => { const t = document.elementFromPoint(${pt.x}, ${pt.y}); if (t) t.dispatchEvent(new MouseEvent('contextmenu', { bubbles: true, cancelable: true, clientX: ${pt.x}, clientY: ${pt.y}, button: 2, buttons: 2 })); })()`);
      }
      return `ok ${step}`;
    }
    case 'key': await pressKey(cdp, val); return `ok ${step}`;
    case 'type': await cdp.send('Input.insertText', { text: val }); return `ok type=<${[...val].length} chars>`;
    case 'text': {
      const t = await cdp.evalJs(`(() => { const e = ${val === 'body' ? 'document.body' : `document.querySelector(${JSON.stringify(val)})`}; return e ? e.innerText : null; })()`);
      if (t === null) throw new Error(`no element for ${JSON.stringify(val)}`);
      return `TEXT ${val}: ${t}`;
    }
    case 'eval': {
      if (/location|sessionStorage|localStorage|token/i.test(val)) throw new Error('eval refused: source mentions a secret-bearing name');
      const v = await cdp.evalJs(val);
      return `EVAL: ${JSON.stringify(v === undefined ? null : v)}`;
    }
    case 'shot': {
      const r = await cdp.send('Page.captureScreenshot', { format: 'png' });
      const out = resolve(val);
      mkdirSync(dirname(out), { recursive: true });
      writeFileSync(out, Buffer.from(r.data, 'base64'));
      return `ok shot=${val}`;
    }
    default: throw new Error(`unknown step "${name}"`);
  }
}

// Kill every msedge.exe whose command line names this throwaway profile (the launched
// msedge.exe can hand off to a different process, so the child handle alone is not enough).
function killProfileBrowsers(udd, proc) {
  try { if (proc && proc.pid) spawnSync('taskkill', ['/F', '/T', '/PID', String(proc.pid)], { stdio: 'ignore', windowsHide: true }); } catch { /* ignore */ }
  try {
    const q = `Get-CimInstance Win32_Process -Filter "Name='msedge.exe'" | Where-Object { $_.CommandLine -and $_.CommandLine.Contains($env:WEBUI_CDP_UDD) } | ForEach-Object { Stop-Process -Id $_.ProcessId -Force -ErrorAction SilentlyContinue }`;
    spawnSync('powershell.exe', ['-NoProfile', '-NonInteractive', '-Command', q], { stdio: 'ignore', windowsHide: true, env: { ...process.env, WEBUI_CDP_UDD: udd } });
  } catch { /* ignore */ }
}

// Edge's network sandbox leaves Cache_Data with an ACL that omits the user, so rmSync
// fails with EPERM; the owner can re-grant itself, then the delete goes through.
async function removeProfile(udd, tries) {
  for (let n = 0; n < tries; n++) {
    try { rmSync(udd, { recursive: true, force: true, maxRetries: 3 }); }
    catch {
      if (n === 2 || n === 8) {
        try { spawnSync('icacls', [udd, '/grant', `${process.env.USERNAME}:(OI)(CI)F`, '/T', '/C', '/Q'], { stdio: 'ignore', windowsHide: true }); } catch { /* retry anyway */ }
      }
      await sleep(250);
    }
    if (!existsSync(udd)) return true;
  }
  return !existsSync(udd);
}

async function main() {
  const udd = mkdtempSync(join(tmpdir(), 'webui-cdp-'));
  let proc = null;
  let cdp = null;
  let code = 1;
  try {
    // --remote-debugging-pipe: CDP over fds 3 (we write) and 4 (we read); no TCP port.
    proc = spawn(EDGE, [
      '--headless=new', '--remote-debugging-pipe', `--user-data-dir=${udd}`,
      '--no-first-run', '--no-default-browser-check', '--window-size=1400,900', 'about:blank',
    ], { stdio: ['ignore', 'ignore', 'ignore', 'pipe', 'pipe'], windowsHide: true });
    proc.on('error', (e) => { console.error('FAIL launch: ' + e.message); });
    let exited = false;
    proc.on('exit', () => { exited = true; });
    cdp = new Cdp(proc.stdio[3], proc.stdio[4]);

    // Wait for the browser to answer on the pipe and show a page target.
    let target = null;
    for (let n = 0; n < 100 && !target; n++) {
      if (exited && cdp.closed) throw new Error('launch: the browser exited and closed the DevTools pipe (a launcher that hands off to another process, e.g. some sandboxed shells, cannot be driven over a pipe; run from a normal shell or WSL)');
      try {
        const r = await Promise.race([cdp.send('Target.getTargets'), sleep(1000).then(() => { throw new Error('slow'); })]);
        target = r.targetInfos.find((t) => t.type === 'page');
      } catch { /* retry */ }
      if (!target) await sleep(100);
    }
    if (!target) throw new Error('launch: no page target on the DevTools pipe in 10 s');
    const att = await cdp.send('Target.attachToTarget', { targetId: target.targetId, flatten: true });
    cdp.sessionId = att.sessionId;
    await cdp.send('Page.enable');
    await cdp.send('Runtime.enable');
    const nav = await cdp.send('Page.navigate', { url: pageUrl(page) });
    if (nav.errorText) throw new Error('navigate: ' + nav.errorText);

    const state = { timeout: 20 };
    for (const step of steps) {
      try {
        console.log(await runStep(cdp, step, state));
      } catch (e) {
        const label = step.startsWith('eval=') ? 'eval=<redacted>' : step;
        console.log(`FAIL ${label}: ${e.message}`);
        return 1;
      }
    }
    code = 0;
  } catch (e) {
    console.log('FAIL ' + e.message);
    code = 1;
  } finally {
    try {
      if (cdp && !cdp.closed) { cdp.sessionId = null; await Promise.race([cdp.send('Browser.close'), sleep(2000)]); }
    } catch { /* closing */ }
    try { if (proc && proc.exitCode === null) { await Promise.race([new Promise((r) => proc.once('exit', r)), sleep(2000)]); } } catch { /* ignore */ }
    killProfileBrowsers(udd, proc);
    let gone = await removeProfile(udd, 12);
    if (!gone) {
      // History may still hold the token URL: kill whatever holds the profile and retry.
      killProfileBrowsers(udd, proc);
      await sleep(500);
      gone = await removeProfile(udd, 12);
    }
    if (!gone) console.log('WARN: throwaway profile ' + basename(udd) + ' was not deleted; its browser history may hold the token URL, delete that dir by hand');
  }
  return code;
}

process.exit(await main());
