// Files put into a message: images go with the say as the door takes them
// (eidolon crates/web/src/attach.rs), text files go inline as a fenced block
// named for the file. Every limit the door would refuse with a 422 is checked
// here first, so the message says why in words before anything is sent.

export const IMAGE_TYPES = ['image/png', 'image/jpeg', 'image/gif', 'image/webp'];
export const MAX_IMAGE = 4 * 1024 * 1024; // attach.rs MAX_BYTES (decoded)
export const MAX_IMAGES = 8;              // attach.rs MAX_IMAGES
export const MAX_BODY = 8 * 1024 * 1024;  // serve.rs MAX_BODY, the whole say
export const MAX_TEXT = 256 * 1024;       // a text file inlined; more is a read's job
const MAX_NAME = 255;

const mb = (n) => (n / 1024 / 1024).toFixed(1) + ' MiB';

// What the bytes are (attach.rs sniff); the door refuses a declared type the bytes contradict.
function sniff(b) {
  const at = (s, o = 0) => [...s].every((c, i) => b[o + i] === c.charCodeAt(0));
  if (b[0] === 0x89 && at('PNG\r\n\x1a\n', 1)) return 'image/png';
  if (b[0] === 0xff && b[1] === 0xd8 && b[2] === 0xff) return 'image/jpeg';
  if (at('GIF87a') || at('GIF89a')) return 'image/gif';
  if (b.length >= 12 && at('RIFF') && at('WEBP', 8)) return 'image/webp';
  return null;
}

function base64(bytes) {
  let s = '';
  for (let i = 0; i < bytes.length; i += 0x8000) s += String.fromCharCode.apply(null, bytes.subarray(i, i + 0x8000));
  return btoa(s);
}

const nameOk = (n) => n && new TextEncoder().encode(n).length <= MAX_NAME && !/[\0/\\]/.test(n);

export const looksImage = (file) => IMAGE_TYPES.includes(file.type) || /\.(png|jpe?g|gif|webp)$/i.test(file.name || '');

// { name, media_type, data, size }, or throws an Error saying why not.
export async function readImage(file) {
  const name = file.name || 'pasted image';
  if (file.size > MAX_IMAGE) throw new Error(`${name}: ${mb(file.size)}, over the door's 4 MiB image limit; shrink it first`);
  const bytes = new Uint8Array(await file.arrayBuffer());
  if (!bytes.length) throw new Error(`${name}: empty`);
  const media_type = sniff(bytes);
  if (!media_type) throw new Error(`${name}: not a PNG, JPEG, GIF or WebP image`);
  return { name: nameOk(name) ? name : 'image', media_type, data: base64(bytes), size: bytes.length };
}

// A text file as a fenced block, or throws an Error saying why not.
export async function readText(file) {
  const name = file.name || 'file';
  if (file.size > MAX_TEXT) throw new Error(`${name}: ${mb(file.size)}, over the ${MAX_TEXT / 1024} KiB inlined-text limit`);
  const bytes = new Uint8Array(await file.arrayBuffer());
  if (bytes.subarray(0, 8192).includes(0)) throw new Error(`${name} looks binary; only text files go inline`);
  let text;
  try { text = new TextDecoder('utf-8', { fatal: true }).decode(bytes); } catch { throw new Error(`${name} is not UTF-8 text`); }
  return '```' + name.replace(/\s+/g, '_') + '\n' + text.replace(/\n$/, '') + '\n```\n';
}

// Why a message with these images would be refused, or ''.
export function tooBig(text, images) {
  if (images.length > MAX_IMAGES) return `at most ${MAX_IMAGES} images go with one message`;
  const size = text.length + images.reduce((n, a) => n + a.data.length + a.name.length + 64, 0);
  return size > MAX_BODY - 4096 ? `${mb(size)} with the images, over the door's 8 MiB message limit` : '';
}
