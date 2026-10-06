// Projects: a folder with a name, kept by the perch (GET/POST /hub/projects, DELETE /hub/projects {path}).
// A chat belongs to a project when its folder is the project's or inside it (the deepest project wins).
// A folderless chat (the inbox) lives in `<home>/scratch/<name>`: a three-word birth name, renamed after its title once it has one.
// That folder is the chat's own, never a project's, and its name is not shown as a project's.
import { signal } from './ui.js';
import { hubGet, hubPost, hubDelete, mode } from './api.js';

export const projects = signal([]); // [{ name, path, created }], in the order the perch holds them (a project is named by its path)

const SCRATCH = /\/scratch\/[^/]+\/?$/;
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

// Give a project a new label (its folder is untouched). Resolves { ok } or { ok: false, why }.
export async function renameProject(p, name) {
  const r = await hubPost('projects/rename', { path: p.path, name });
  if (r.status === 200) { await loadProjects(); return { ok: true }; }
  const why = r.status === 400 ? (r.data && r.data.error) || 'that name will not do'
    : r.status === 404 ? 'that project is gone'
    : r.status === 0 ? 'the server is not answering'
    : (r.data && r.data.error) || `could not rename it (${r.status})`;
  return { ok: false, why };
}

// Forget a project (its folder and files stay). `p` is an entry of `projects`; the perch removes it by its path, so a
// stale list in another tab cannot remove a different one.
export async function forgetProject(p) {
  const r = await hubDelete('projects', { path: p.path });
  await loadProjects();
  return r.status === 204;
}
