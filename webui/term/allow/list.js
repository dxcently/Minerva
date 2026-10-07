// The loaded tab's `allowed here` section: the allow rules the chat holds, each with a way to revoke it. Empty until slice A2 fills it; it
// registers like any section (core/registry.js), so app.js lists nothing of it.
import { loadedSections } from '../core/registry.js';

export const AllowList = () => null;

loadedSections.add(AllowList);
