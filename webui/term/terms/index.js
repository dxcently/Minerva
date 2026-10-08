// The terminals' folder, loaded by app.js: the ⌨ tab in the right panel (tab.js), the screens (view.js), and what an agent does in a
// terminal while the tab is not showing (activity.js: the rail's badge, the toast, the frame tap, the 5 s roster poll). It registers like any
// tab (core/registry.js), so app.js lists nothing of it; global, so it is drawn with no chat in view.
import { rightTabs, frameTaps } from '../core/registry.js';
import { TermsTab } from './tab.js';
import { badge, tap, startActivity } from './activity.js';
import './view.js';

rightTabs.add({ name: 'terms', icon: '⌨︎', component: TermsTab, global: true, badge });
frameTaps.add(tap);
startActivity();
