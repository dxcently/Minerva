//! The one shared browser and page, and the six methods that act on them.
//!
//! Everything here runs under the service's single lock (see `main.rs`), so
//! "the current snapshot" can't change in the middle of a call.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chromiumoxide::cdp::browser_protocol::accessibility::GetFullAxTreeParams;
use chromiumoxide::cdp::browser_protocol::browser::{
    BrowserContextId, CloseParams, SetDownloadBehaviorBehavior, SetDownloadBehaviorParams,
};
use chromiumoxide::cdp::browser_protocol::dom::{
    BackendNodeId, GetContentQuadsParams, GetNodeForLocationParams, ResolveNodeParams,
    ScrollIntoViewIfNeededParams,
};
use chromiumoxide::cdp::browser_protocol::input::{
    DispatchKeyEventParams, DispatchKeyEventType, DispatchMouseEventParams, DispatchMouseEventType,
    InsertTextParams, MouseButton,
};
use chromiumoxide::cdp::browser_protocol::page::{
    DialogType, EventDomContentEventFired, EventFrameNavigated, EventFrameStartedLoading,
    EventFrameStoppedLoading, EventJavascriptDialogOpening, EventLoadEventFired,
    GetFrameTreeParams, GetNavigationHistoryParams, HandleJavaScriptDialogParams, NavigateParams,
    NavigateToHistoryEntryParams,
};
use chromiumoxide::cdp::browser_protocol::target::{
    CloseTargetParams, CreateBrowserContextParams, CreateTargetParams, EventTargetCreated,
};
use chromiumoxide::cdp::js_protocol::runtime::{CallArgument, CallFunctionOnParams};
use chromiumoxide::handler::viewport::Viewport;
use chromiumoxide::{Browser, BrowserConfig, Page};
use futures::StreamExt;
use serde_json::{Value, json};
use tokio::task::JoinHandle;

use crate::filter::{filter_roles, first_heading, head, refs_in};
use crate::origin::{origin_of, parse_confine};
use crate::proxy::Proxy;
use crate::snapshot::{AxItem, render, with_role};
use crate::sweep::make_profile;
use crate::text::{quote, quote_list, repr, truncate_chars};

const ACTION_TIMEOUT: Duration = Duration::from_secs(8);
const NAV_TIMEOUT_S: f64 = 30.0;
const SNAPSHOT_TIMEOUT_S: f64 = 60.0;

type Res = Result<Value, String>;

struct Live {
    browser: Arc<Browser>,
    profile: PathBuf,
    tasks: Vec<JoinHandle<()>>,
}

pub struct State {
    chrome: Option<PathBuf>,
    proxy: Proxy,
    live: Option<Live>,
    context: Option<BrowserContextId>,
    page: Option<Page>,
    dialogs: Option<JoinHandle<()>>,
    /// The context popups get closed in (read by the popup task).
    current: Arc<Mutex<Option<BrowserContextId>>>,
    refs: HashMap<String, BackendNodeId>,
    next_ref: u64,
    confined: Option<Vec<String>>,
}

impl State {
    pub fn new(chrome: Option<PathBuf>, proxy: Proxy) -> State {
        State {
            chrome,
            proxy,
            live: None,
            context: None,
            page: None,
            dialogs: None,
            current: Arc::new(Mutex::new(None)),
            refs: HashMap::new(),
            next_ref: 0,
            confined: None,
        }
    }

    pub async fn call(&mut self, method: &str, args: &Value) -> Res {
        match method {
            "open" => self.open(args).await,
            "snapshot" => self.snapshot(args).await,
            "click" => self.click(args).await,
            "type" => self.type_text(args).await,
            "read" => self.read(args).await,
            "back" => self.back(args).await,
            _ => unreachable!("main.rs only passes known methods"),
        }
    }

    // --- lifecycle -------------------------------------------------------

    async fn ensure_browser(&mut self) -> Result<Arc<Browser>, String> {
        if let Some(live) = &self.live {
            return Ok(live.browser.clone());
        }
        let chrome = self
            .chrome
            .clone()
            .ok_or("no Chromium found: set EIDOLON_BROWSER_CHROME, or put chromium on PATH")?;
        let profile = make_profile(&std::env::temp_dir())
            .map_err(|e| format!("could not make a profile directory: {e}"))?;
        let mut builder = BrowserConfig::builder()
            .chrome_executable(&chrome)
            .new_headless_mode()
            .user_data_dir(&profile)
            .viewport(Viewport {
                width: 1280,
                height: 720,
                device_scale_factor: Some(1.0),
                ..Default::default()
            })
            .request_timeout(Duration::from_secs(120))
            .arg("--disable-back-forward-cache");
        if std::env::var_os("EIDOLON_BROWSER_NO_SANDBOX").is_some() {
            builder = builder.no_sandbox();
        }
        let config = builder.build()?;
        let (browser, mut handler) = Browser::launch(config)
            .await
            .map_err(|e| format!("could not launch {}: {e}", chrome.display()))?;
        let browser = Arc::new(browser);
        let mut tasks = vec![tokio::spawn(async move {
            while handler.next().await.is_some() {}
        })];

        // Popups (target=_blank, window.open) are closed as they appear: the
        // service drives one page, and a page nobody drives is only a leak.
        let mut created = browser
            .event_listener::<EventTargetCreated>()
            .await
            .map_err(|e| e.to_string())?;
        let (b, current) = (browser.clone(), self.current.clone());
        tasks.push(tokio::spawn(async move {
            while let Some(ev) = created.next().await {
                let info = &ev.target_info;
                let ours = current.lock().unwrap().clone();
                if info.r#type == "page"
                    && info.opener_id.is_some()
                    && ours.is_some()
                    && info.browser_context_id == ours
                {
                    let _ = b
                        .execute(CloseTargetParams::new(info.target_id.clone()))
                        .await;
                }
            }
        }));

        self.live = Some(Live {
            browser: browser.clone(),
            profile,
            tasks,
        });
        Ok(browser)
    }

    /// Replace the context (and with it the page and every cookie). A
    /// confined context sends all its traffic through the proxy.
    async fn new_context(&mut self, confined: bool) -> Result<(), String> {
        let browser = self.ensure_browser().await?;
        if let Some(task) = self.dialogs.take() {
            task.abort();
        }
        self.page = None;
        if let Some(old) = self.context.take() {
            let _ = browser.dispose_browser_context(old).await;
        }
        self.refs.clear();

        let mut params = CreateBrowserContextParams::default();
        if confined {
            params.proxy_server = Some(format!("http://{}", self.proxy.addr));
            // Loopback skips a proxy by default; `<-loopback>` takes that back.
            params.proxy_bypass_list = Some("<-loopback>".into());
        }
        let ctx = browser
            .create_browser_context(params)
            .await
            .map_err(|e| format!("could not make a browser context: {e}"))?;
        *self.current.lock().unwrap() = Some(ctx.clone());

        let mut deny = SetDownloadBehaviorParams::new(SetDownloadBehaviorBehavior::Deny);
        deny.browser_context_id = Some(ctx.clone());
        browser.execute(deny).await.map_err(|e| e.to_string())?;

        let mut target = CreateTargetParams::new("about:blank");
        target.browser_context_id = Some(ctx.clone());
        let page = browser
            .new_page(target)
            .await
            .map_err(|e| format!("could not open a page: {e}"))?;

        // An alert() would block the page, and every call after it, forever.
        let mut dialogs = page
            .event_listener::<EventJavascriptDialogOpening>()
            .await
            .map_err(|e| e.to_string())?;
        let p = page.clone();
        self.dialogs = Some(tokio::spawn(async move {
            while let Some(ev) = dialogs.next().await {
                let accept = matches!(ev.r#type, DialogType::Beforeunload);
                let _ = p.execute(HandleJavaScriptDialogParams::new(accept)).await;
            }
        }));

        self.context = Some(ctx);
        self.page = Some(page);
        Ok(())
    }

    async fn ensure_page(&mut self) -> Result<Page, String> {
        if self.page.is_none() {
            self.new_context(false).await?;
        }
        Ok(self.page.clone().expect("new_context sets a page"))
    }

    /// Shut Chromium down and remove its profile.
    pub async fn close(&mut self) {
        if let Some(task) = self.dialogs.take() {
            task.abort();
        }
        self.page = None;
        self.context = None;
        self.refs.clear();
        self.confined = None;
        let Some(live) = self.live.take() else {
            return;
        };
        let _ = tokio::time::timeout(
            Duration::from_secs(10),
            live.browser.execute(CloseParams::default()),
        )
        .await;
        // The popup task holds a handle to the browser; it has to be gone
        // before the browser can be waited on.
        for task in live.tasks {
            task.abort();
            let _ = task.await;
        }
        if let Ok(mut browser) = Arc::try_unwrap(live.browser)
            && tokio::time::timeout(Duration::from_secs(10), browser.wait())
                .await
                .is_err()
        {
            let _ = browser.kill().await;
            let _ = browser.wait().await;
        }
        // Chrome's helpers can outlive the main process by a moment.
        for _ in 0..20 {
            if std::fs::remove_dir_all(&live.profile).is_ok() || !live.profile.exists() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    // --- shared steps ----------------------------------------------------

    /// Where the page is and what it's called.
    async fn info(&self, page: &Page) -> (String, String) {
        for _ in 0..10 {
            if let Ok(r) = page.evaluate("[location.href, document.title]").await
                && let Ok((url, title)) = r.into_value::<(String, String)>()
            {
                return (url, title);
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        (String::new(), String::new())
    }

    fn confined_json(&self) -> Value {
        self.confined.as_ref().map_or(Value::Null, |c| json!(c))
    }

    fn confined_list(&self) -> String {
        quote_list(self.confined.as_deref().unwrap_or_default())
    }

    /// Navigate and wait for the new document's `load`.
    async fn navigate(page: &Page, url: &str, timeout_s: f64) -> Result<(), String> {
        let mut loaded = page
            .event_listener::<EventLoadEventFired>()
            .await
            .map_err(|e| e.to_string())?;
        let nav = page
            .execute(NavigateParams::new(url))
            .await
            .map_err(|e| format!("navigating to {url}: {e}"))?;
        if let Some(err) = &nav.result.error_text {
            return Err(format!("navigating to {url}: {err}"));
        }
        if nav.result.loader_id.is_none() {
            return Ok(()); // same document: a fragment change
        }
        tokio::time::timeout(Duration::from_secs_f64(timeout_s), loaded.next())
            .await
            .map(|_| ())
            .map_err(|_| format!("navigating to {url}: no load event within {timeout_s}s"))
    }

    fn take_ref(&mut self, args: &Value) -> Result<(String, BackendNodeId), String> {
        let r = args
            .get("ref")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .ok_or("needs a ref from the current browser_snapshot")?;
        let node = self.refs.get(r).cloned().ok_or_else(|| {
            format!(
                "ref {} is not in the current snapshot; call browser_snapshot again",
                quote(r)
            )
        })?;
        Ok((r.to_string(), node))
    }

    /// After a click or Enter: if a navigation starts within half a second,
    /// wait for its DOMContentLoaded.
    async fn settle(
        started: &mut (impl futures::Stream + Unpin),
        ready: &mut (impl futures::Stream + Unpin),
    ) {
        if tokio::time::timeout(Duration::from_millis(500), started.next())
            .await
            .is_ok()
        {
            let _ = tokio::time::timeout(ACTION_TIMEOUT, ready.next()).await;
        }
    }

    /// In a confined context, did the page's last navigation fail on an
    /// origin that isn't on the list? Chrome leaves a failed navigation on
    /// its error page with the URL it couldn't load, so that is read here.
    /// An allowed origin that was merely down is not "blocked".
    async fn blocked_navigation(&self, page: &Page, what: &str) -> Result<(), String> {
        let Some(allowed) = &self.confined else {
            return Ok(());
        };
        let Ok(tree) = page.execute(GetFrameTreeParams::default()).await else {
            return Ok(());
        };
        let Some(url) = tree.result.frame_tree.frame.unreachable_url.clone() else {
            return Ok(());
        };
        let origin = origin_of(&url).unwrap_or_else(|_| url.clone());
        if allowed.contains(&origin) {
            return Ok(());
        }
        Err(format!(
            "{what}: navigation to {url} was blocked -- origin {origin} is not in confine {}",
            self.confined_list()
        ))
    }

    // --- open --------------------------------------------------------------

    async fn open(&mut self, args: &Value) -> Res {
        let url = args
            .get("url")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or("open needs a url")?
            .to_string();
        let timeout_s = seconds(args.get("timeout_s"), NAV_TIMEOUT_S, "timeout_s")?;

        match args.get("confine").filter(|v| !v.is_null()) {
            Some(v) => {
                let origins = parse_confine(v)?;
                self.proxy.allow(origins.clone());
                self.new_context(true).await?;
                self.confined = Some(origins);
            }
            None if self.confined.is_some() => {
                self.new_context(false).await?;
                self.confined = None;
            }
            None => {}
        }
        let page = self.ensure_page().await?;

        if let Some(allowed) = &self.confined {
            let origin = origin_of(&url)?;
            if !allowed.contains(&origin) {
                return Err(format!(
                    "open {url}: origin {origin} is not in confine {}",
                    self.confined_list()
                ));
            }
        }

        if let Err(e) = Self::navigate(&page, &url, timeout_s).await {
            self.blocked_navigation(&page, &format!("open {url}"))
                .await?;
            return Err(e);
        }
        self.refs.clear();
        let (url, title) = self.info(&page).await;
        Ok(json!({ "url": url, "title": title, "confined": self.confined_json() }))
    }

    // --- snapshot ----------------------------------------------------------

    async fn snapshot(&mut self, args: &Value) -> Res {
        let timeout_s = seconds(args.get("timeout_s"), SNAPSHOT_TIMEOUT_S, "timeout_s")?;
        let depth = match args.get("depth").filter(|v| !v.is_null()) {
            None => None,
            Some(v) => Some(v.as_u64().ok_or("depth must be a non-negative integer")? as usize),
        };
        let within = match args.get("within").filter(|v| !v.is_null()) {
            None => None,
            Some(v) => Some(
                v.as_str()
                    .filter(|s| !s.is_empty())
                    .ok_or(r#"within must be a landmark role name, e.g. "main""#)?
                    .to_string(),
            ),
        };
        let roles = args.get("roles").filter(|v| !v.is_null());
        let max = args.get("max").filter(|v| !v.is_null());
        let section = args.get("section").filter(|v| !v.is_null());

        let (roles, max, section) = match roles {
            None => {
                if max.is_some() {
                    return Err("max requires roles -- it caps the filtered list; there is no such cap for a whole tree".into());
                }
                if section.is_some() {
                    return Err("section requires roles -- it narrows a role-filtered list; there is no such list for a whole tree".into());
                }
                (None, None, None)
            }
            Some(r) => {
                let roles: Vec<String> = r
                    .as_array()
                    .filter(|a| !a.is_empty())
                    .and_then(|a| {
                        a.iter()
                            .map(|x| x.as_str().filter(|s| !s.is_empty()).map(str::to_string))
                            .collect()
                    })
                    .ok_or(r#"roles must be a non-empty list of role names, e.g. ["link"]"#)?;
                let max = match max {
                    None => None,
                    Some(m) => Some(
                        m.as_u64()
                            .filter(|&n| n > 0 && m.is_u64())
                            .ok_or("max must be a positive integer")?
                            as usize,
                    ),
                };
                let section = match section {
                    None => None,
                    Some(s) => Some(
                        s.as_str()
                            .filter(|s| !s.is_empty())
                            .ok_or(r#"section must be a non-empty string naming a heading, e.g. "See also""#)?
                            .to_string(),
                    ),
                };
                if section.is_some() && within.is_none() {
                    return Err("section requires within -- a section is one of within's own nested landmarks, and there is none to be inside without within".into());
                }
                (Some(roles), max, section)
            }
        };

        let page = self.ensure_page().await?;
        let tree = tokio::time::timeout(
            Duration::from_secs_f64(timeout_s),
            page.execute(GetFullAxTreeParams::default()),
        )
        .await
        .map_err(|_| format!("snapshot: no accessibility tree within {timeout_s}s"))?
        .map_err(|e| format!("snapshot: {e}"))?;
        let nodes = serde_json::to_value(&tree.result.nodes).map_err(|e| e.to_string())?;
        let items = ax_items(&nodes);
        let (url, title) = self.info(&page).await;

        let mut minted: HashMap<String, BackendNodeId> = HashMap::new();
        let mut counter = self.next_ref;
        let mut mint = |backend: i64| {
            counter += 1;
            let r = format!("e{counter}");
            minted.insert(r.clone(), BackendNodeId::new(backend));
            r
        };
        let mut text = match &within {
            Some(role) => {
                let found = with_role(&items, role);
                if found.len() != 1 {
                    return Err(format!(
                        "within={}: {} elements with that role on {url}; a scope names exactly one{}",
                        quote(role),
                        found.len(),
                        if found.len() > 1 {
                            " -- use an unscoped snapshot, or depth"
                        } else {
                            ""
                        }
                    ));
                }
                render(&items, found[0], true, depth, &mut mint)
            }
            None => match items.iter().position(|i| i.role == "RootWebArea") {
                Some(root) => render(&items, root, false, depth, &mut mint),
                None => String::new(),
            },
        };
        self.next_ref = counter;

        let heading = first_heading(&text);
        if let Some(roles) = &roles {
            let set: HashSet<String> = roles.iter().cloned().collect();
            text = filter_roles(
                &text,
                &set,
                within.as_deref(),
                max,
                section.as_deref(),
                &url,
            )?;
        }
        let kept: HashSet<String> = refs_in(&text).into_iter().collect();
        minted.retain(|r, _| kept.contains(r));
        self.refs = minted;

        Ok(Value::String(
            head(
                &url,
                &title,
                heading.as_deref(),
                within.as_deref(),
                roles.as_deref(),
                section.as_deref(),
                text.chars().count(),
            ) + &text,
        ))
    }

    // --- click -------------------------------------------------------------

    async fn click(&mut self, args: &Value) -> Res {
        let (r, node) = self.take_ref(args)?;
        let page = self.ensure_page().await?;
        self.refs.clear();

        let point = match locate(&page, &node).await {
            Some(p) => p,
            None => {
                return Err(format!(
                    "ref {} was in the last snapshot but the page would not resolve it to a clickable element; it may have changed underneath -- call browser_snapshot again",
                    quote(&r)
                ));
            }
        };
        let mut started = page
            .event_listener::<EventFrameStartedLoading>()
            .await
            .map_err(|e| e.to_string())?;
        let mut ready = page
            .event_listener::<EventDomContentEventFired>()
            .await
            .map_err(|e| e.to_string())?;
        for kind in [
            DispatchMouseEventType::MouseMoved,
            DispatchMouseEventType::MousePressed,
            DispatchMouseEventType::MouseReleased,
        ] {
            let mut ev = DispatchMouseEventParams::new(kind.clone(), point.0, point.1);
            if kind != DispatchMouseEventType::MouseMoved {
                ev.button = Some(MouseButton::Left);
                ev.click_count = Some(1);
            }
            page.execute(ev)
                .await
                .map_err(|e| format!("click {r}: {e}"))?;
        }
        Self::settle(&mut started, &mut ready).await;

        let (url, title) = self.info(&page).await;
        self.blocked_navigation(&page, &format!("click {r}"))
            .await?;
        Ok(json!({ "ref": r, "url": url, "title": title, "confined": self.confined_json() }))
    }

    // --- type --------------------------------------------------------------

    async fn type_text(&mut self, args: &Value) -> Res {
        let text = args
            .get("text")
            .and_then(Value::as_str)
            .ok_or("type needs text")?
            .to_string();
        let (r, node) = self.take_ref(args)?;
        let page = self.ensure_page().await?;
        self.refs.clear();

        match focus_for_typing(&page, &node).await {
            Ok(()) => {}
            Err(None) => {
                return Err(format!(
                    "ref {} was in the last snapshot but the page would not resolve it to a fillable element; it may have changed underneath -- call browser_snapshot again",
                    quote(&r)
                ));
            }
            Err(Some(why)) => return Err(format!("type {r}: {why}")),
        }
        if text.is_empty() {
            key(&page, "Delete", 46, None).await?;
        } else {
            page.execute(InsertTextParams::new(text))
                .await
                .map_err(|e| format!("type {r}: {e}"))?;
        }

        if truthy(args.get("submit")) {
            let mut started = page
                .event_listener::<EventFrameStartedLoading>()
                .await
                .map_err(|e| e.to_string())?;
            let mut ready = page
                .event_listener::<EventDomContentEventFired>()
                .await
                .map_err(|e| e.to_string())?;
            key(&page, "Enter", 13, Some("\r")).await?;
            Self::settle(&mut started, &mut ready).await;
        }

        let (url, title) = self.info(&page).await;
        self.blocked_navigation(&page, &format!("type {r}")).await?;
        Ok(json!({ "ref": r, "url": url, "title": title, "confined": self.confined_json() }))
    }

    // --- read --------------------------------------------------------------

    async fn read(&mut self, args: &Value) -> Res {
        let max = match args.get("max") {
            None | Some(Value::Null) => 8000,
            Some(v) => {
                let n = v
                    .as_f64()
                    .or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))
                    .ok_or("max must be a positive integer")?;
                if n < 0.0 {
                    return Err("max must be a positive integer".into());
                }
                if n < 1.0 { 8000 } else { n as usize }
            }
        };
        let page = self.ensure_page().await?;
        let body: Option<String> = page
            .evaluate("document.body ? document.body.innerText : null")
            .await
            .map_err(|e| format!("read: {e}"))?
            .into_value()
            .map_err(|e| format!("read: {e}"))?;
        let body = body.ok_or("read: the page has no body")?;
        let (text, truncated) = truncate_chars(&body, max);
        let (url, title) = self.info(&page).await;
        Ok(json!({ "url": url, "title": title, "text": text, "truncated": truncated }))
    }

    // --- back --------------------------------------------------------------

    async fn back(&mut self, _args: &Value) -> Res {
        let page = self.ensure_page().await?;
        let history = page
            .execute(GetNavigationHistoryParams::default())
            .await
            .map_err(|e| format!("back: {e}"))?;
        let idx = history.result.current_index;
        // A new page's first entry is the about:blank it was created on,
        // which is not somewhere anyone went.
        let first_is_blank = history
            .result
            .entries
            .first()
            .is_some_and(|e| e.url == "about:blank");
        let went_back = idx > 1 || (idx == 1 && !first_is_blank);
        if went_back {
            let entries = &history.result.entries;
            let (here, there) = (&entries[idx as usize], &entries[idx as usize - 1]);
            let same_document = strip_fragment(&here.url) == strip_fragment(&there.url);
            // Chromium 154 delivers no `Page.loadEventFired` for a history
            // navigation that lands on a network document — nor, measured
            // the same way, DomContentEventFired or FrameStartedLoading —
            // while a data: entry still delivers them all (which is why the
            // rig never saw this). So wait for the FIRST of every completion
            // signal the session still sends; whichever arrives, the entry
            // has navigated.
            let mut load = page
                .event_listener::<EventLoadEventFired>()
                .await
                .map_err(|e| e.to_string())?;
            let mut ready = page
                .event_listener::<EventDomContentEventFired>()
                .await
                .map_err(|e| e.to_string())?;
            let mut navigated = page
                .event_listener::<EventFrameNavigated>()
                .await
                .map_err(|e| e.to_string())?;
            let mut stopped = page
                .event_listener::<EventFrameStoppedLoading>()
                .await
                .map_err(|e| e.to_string())?;
            page.execute(NavigateToHistoryEntryParams::new(there.id))
                .await
                .map_err(|e| format!("back: {e}"))?;
            if !same_document {
                tokio::time::timeout(Duration::from_secs_f64(NAV_TIMEOUT_S), async {
                    tokio::select! {
                        _ = load.next() => (),
                        _ = ready.next() => (),
                        _ = navigated.next() => (),
                        _ = stopped.next() => (),
                    }
                })
                .await
                .map(|_| ())
                .map_err(|_| {
                    format!("back: the history entry did not finish loading within {NAV_TIMEOUT_S}s")
                })?;
            }
        }
        self.refs.clear();
        let (url, title) = self.info(&page).await;
        Ok(json!({ "went_back": went_back, "url": url, "title": title }))
    }
}

// --- page-level helpers --------------------------------------------------

/// Chrome's accessibility nodes, as the renderer's items.
fn ax_items(nodes: &Value) -> Vec<AxItem> {
    let nodes = nodes.as_array().cloned().unwrap_or_default();
    let index: HashMap<String, usize> = nodes
        .iter()
        .enumerate()
        .filter_map(|(i, n)| Some((n.get("nodeId")?.as_str()?.to_string(), i)))
        .collect();
    let value = |n: &Value, key: &str| {
        n.get(key)
            .and_then(|v| v.get("value"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    nodes
        .iter()
        .map(|n| AxItem {
            role: value(n, "role"),
            name: value(n, "name"),
            ignored: n.get("ignored").and_then(Value::as_bool).unwrap_or(false),
            children: n
                .get("childIds")
                .and_then(Value::as_array)
                .map(|ids| {
                    ids.iter()
                        .filter_map(|id| index.get(id.as_str()?).copied())
                        .collect()
                })
                .unwrap_or_default(),
            backend: n.get("backendDOMNodeId").and_then(Value::as_i64),
            props: n
                .get("properties")
                .and_then(Value::as_array)
                .map(|ps| {
                    ps.iter()
                        .filter_map(|p| {
                            Some((
                                p.get("name")?.as_str()?.to_string(),
                                p.get("value")?.get("value")?.clone(),
                            ))
                        })
                        .collect()
                })
                .unwrap_or_default(),
        })
        .collect()
}

/// Is `node` there, visible, and the thing a click at its centre would hit?
/// Retried for up to [`ACTION_TIMEOUT`]; `None` when it never was, or when
/// the node is gone for good.
async fn locate(page: &Page, node: &BackendNodeId) -> Option<(f64, f64)> {
    let deadline = tokio::time::Instant::now() + ACTION_TIMEOUT;
    loop {
        match try_locate(page, node).await {
            Ok(Some(p)) => return Some(p),
            Ok(None) => {}
            Err(e) if e.contains("No node") => return None,
            Err(_) => {}
        }
        if tokio::time::Instant::now() >= deadline {
            return None;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn try_locate(page: &Page, node: &BackendNodeId) -> Result<Option<(f64, f64)>, String> {
    page.execute(scroll_to(node))
        .await
        .map_err(|e| e.to_string())?;

    let quads = GetContentQuadsParams {
        backend_node_id: Some(*node),
        ..Default::default()
    };
    let quads = page.execute(quads).await.map_err(|e| e.to_string())?;
    let Some(q) = quads
        .result
        .quads
        .iter()
        .map(|q| q.inner().clone())
        .find(|q| q.len() == 8 && area(q) > 1.0)
    else {
        return Ok(None);
    };
    let (x, y) = (
        (q[0] + q[2] + q[4] + q[6]) / 4.0,
        (q[1] + q[3] + q[5] + q[7]) / 4.0,
    );

    // Hit test: whatever is on top at that point must be the node or inside it.
    let mut hit = GetNodeForLocationParams::new(x as i64, y as i64);
    hit.include_user_agent_shadow_dom = Some(true);
    let hit = page.execute(hit).await.map_err(|e| e.to_string())?;
    if hit.result.backend_node_id == *node {
        return Ok(Some((x, y)));
    }
    let target = object_id(page, node).await?;
    let top = object_id(page, &hit.result.backend_node_id).await?;
    let mut contains =
        CallFunctionOnParams::new("function(o) { return this === o || this.contains(o); }");
    contains.object_id = Some(target);
    contains.arguments = Some(vec![CallArgument {
        object_id: Some(top),
        ..Default::default()
    }]);
    contains.return_by_value = Some(true);
    let r = page.execute(contains).await.map_err(|e| e.to_string())?;
    Ok((r.result.result.value == Some(Value::Bool(true))).then_some((x, y)))
}

fn area(q: &[f64]) -> f64 {
    ((q[0] * q[3] - q[2] * q[1])
        + (q[2] * q[5] - q[4] * q[3])
        + (q[4] * q[7] - q[6] * q[5])
        + (q[6] * q[1] - q[0] * q[7]))
        .abs()
        / 2.0
}

async fn object_id(
    page: &Page,
    node: &BackendNodeId,
) -> Result<chromiumoxide::cdp::js_protocol::runtime::RemoteObjectId, String> {
    let resolve = ResolveNodeParams {
        backend_node_id: Some(*node),
        ..Default::default()
    };
    let r = page.execute(resolve).await.map_err(|e| e.to_string())?;
    r.result
        .object
        .object_id
        .clone()
        .ok_or_else(|| "No node object".to_string())
}

fn scroll_to(node: &BackendNodeId) -> ScrollIntoViewIfNeededParams {
    ScrollIntoViewIfNeededParams {
        backend_node_id: Some(*node),
        ..Default::default()
    }
}

const FOCUS_FOR_TYPING: &str = r#"function() {
  const el = this;
  if (el instanceof HTMLInputElement) {
    const never = ['checkbox', 'radio', 'file', 'button', 'submit', 'reset', 'image', 'range', 'color', 'hidden'];
    if (never.includes(el.type)) return 'input of type "' + el.type + '" cannot be filled';
    if (el.disabled || el.readOnly) return 'the element is not editable';
    el.focus(); el.select(); return '';
  }
  if (el instanceof HTMLTextAreaElement) {
    if (el.disabled || el.readOnly) return 'the element is not editable';
    el.focus(); el.select(); return '';
  }
  if (el.isContentEditable) {
    el.focus();
    const range = document.createRange(); range.selectNodeContents(el);
    const sel = getSelection(); sel.removeAllRanges(); sel.addRange(range);
    return '';
  }
  return 'the element is not an <input>, <textarea> or [contenteditable] element';
}"#;

/// Focus the node and select its contents, so what's typed replaces them.
/// `Err(None)` when the node can't be found; `Err(Some(why))` when it can,
/// but isn't something you type into.
async fn focus_for_typing(page: &Page, node: &BackendNodeId) -> Result<(), Option<String>> {
    let deadline = tokio::time::Instant::now() + ACTION_TIMEOUT;
    loop {
        let attempt = async {
            let _ = page.execute(scroll_to(node)).await;
            let id = object_id(page, node).await?;
            let mut call = CallFunctionOnParams::new(FOCUS_FOR_TYPING);
            call.object_id = Some(id);
            call.return_by_value = Some(true);
            let r = page.execute(call).await.map_err(|e| e.to_string())?;
            Ok::<_, String>(
                r.result
                    .result
                    .value
                    .and_then(|v| v.as_str().map(str::to_string))
                    .unwrap_or_default(),
            )
        };
        match attempt.await {
            Ok(why) if why.is_empty() => return Ok(()),
            Ok(why) => return Err(Some(why)),
            Err(e) if e.contains("No node") => return Err(None),
            Err(_) => {}
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(None);
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn key(page: &Page, name: &str, code: i64, text: Option<&str>) -> Result<(), String> {
    for kind in [DispatchKeyEventType::KeyDown, DispatchKeyEventType::KeyUp] {
        let mut ev = DispatchKeyEventParams::new(kind.clone());
        ev.key = Some(name.into());
        ev.code = Some(name.into());
        ev.windows_virtual_key_code = Some(code);
        ev.native_virtual_key_code = Some(code);
        if kind == DispatchKeyEventType::KeyDown {
            ev.text = text.map(str::to_string);
        }
        page.execute(ev)
            .await
            .map_err(|e| format!("pressing {name}: {e}"))?;
    }
    Ok(())
}

// --- argument helpers ------------------------------------------------------

/// Seconds from a number or numeric string; missing, null or 0 means the
/// default.
fn seconds(v: Option<&Value>, default: f64, name: &str) -> Result<f64, String> {
    let n = match v {
        None | Some(Value::Null) => return Ok(default),
        Some(v) => v
            .as_f64()
            .or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))
            .ok_or_else(|| format!("{name} must be a number of seconds; got {}", repr(v)))?,
    };
    if n < 0.0 {
        return Err(format!("{name} must be a number of seconds; got {n}"));
    }
    Ok(if n == 0.0 { default } else { n })
}

pub fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) | Some(Value::Bool(false)) => false,
        Some(Value::Number(n)) => n.as_f64() != Some(0.0),
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
        Some(Value::Bool(true)) => true,
    }
}

fn strip_fragment(url: &str) -> &str {
    url.split('#').next().unwrap_or(url)
}
