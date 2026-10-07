// Hand a chat off to another node: a line in the sidebar chat menus that drafts a note (title, project, folder, the last reply quoted, an
// empty "what's next:") and opens it. Today only the seam; slice N2b fills this file. It registers itself (chatMenuItems.add,
// core/registry.js) when index.js imports it, and needs nothing exported. From store.js it wants `createNote({ title, text })` and
// `openNote(name, { fresh: true })`.
