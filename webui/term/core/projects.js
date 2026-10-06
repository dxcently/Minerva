// Projects: a folder with a name, kept by the perch (GET/POST /hub/projects, DELETE /hub/projects/<n>).
// A chat belongs to a project when its folder is the project's or inside it (the deepest project wins).
// A folderless chat lives in a scratch folder whose name is a time stamp: that name is never shown.
import { signal } from './ui.js';
import { hubGet, hubPost, hubDelete, mode } from './api.js';

export const projects = signal([]); // [{ name, path, created }], in the order the perch holds them (the index is the id)

const SCRATCH = /\/scratch\/\d{8}-\d{6}(-\d+)?$/;
export const isScratch = (cwd) => SCRATCH.test(String(cwd || ''));

export async function loadProjects() {
  if (mode() !== 'perch') return;
  const { status, data } = await hubGet('projects', 5000);
  if (status === 200 && Array.isArray(data)) projects.value = data;
}

const inside = (cwd, base) => cwd === base || cwd.startsWith(base.endsWith('/') ? base : base + '/');

// The project a folder is in, or null.
export function projectFor(cwd, list = projects.value) {
  const c = String(cwd || '');
  if (!c) return null;
  let best = null;
  for (const p of list) if (inside(c, p.path) && (!best || p.path.length > best.path.length)) best = p;
  return best;
}

// Alphabetical, as the sidebar shows them.
export const sorted = (list = projects.value) => [...list].sort((a, b) => a.name.localeCompare(b.name, undefined, { sensitivity: 'base' }) || a.path.localeCompare(b.path));

// Remember a folder as a project. Resolves { ok, project } or { ok: false, why } (a sentence).
export async function addProject(path, name) {
  const r = await hubPost('projects', { path, ...(name ? { name } : {}) });
  if ((r.status === 201 || r.status === 200) && r.data && r.data.path) { await loadProjects(); return { ok: true, project: r.data, existed: r.status === 200 }; }
  const why = r.status === 422 ? 'that folder is not inside the folders this server may open'
    : r.status === 400 ? (r.data && r.data.error) || 'that name will not do'
    : r.status === 0 ? 'the server is not answering'
    : (r.data && r.data.error) || `could not save the project (${r.status})`;
  return { ok: false, why };
}

// Forget a project (its folder and files stay). `p` is an entry of `projects`.
export async function forgetProject(p) {
  const n = projects.peek().findIndex((x) => x.path === p.path);
  if (n < 0) return false;
  const r = await hubDelete(`projects/${n}`);
  await loadProjects();
  return r.status === 204;
}
