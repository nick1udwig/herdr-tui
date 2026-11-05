mod git;
mod interaction;
mod notifications;
mod render;
use crate::{
    Host, Result, actions,
    copy::{Copy, clipboard},
    input,
    layout::{self, Pane},
    paint, preferences,
    shell::{
        self, Hits, Mode,
        chrome::ChromeState,
        keys::Keymap,
        overlays::{Overlay, OverlayRender},
        snapshot::{self, GitInfo, Snapshot},
    },
    vt::Vt,
};
use crossterm::event::{
    Event, EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind,
};
use futures_util::StreamExt;
use herdr_client_lib::{
    client::{Error, Stream, StreamEvent},
    commands::qualify,
    model,
    protocol::{MachineList, Notification},
    sync::Session,
    transport::HcConfig,
    view::View,
};
use ratatui::{Terminal, backend::CrosstermBackend, layout::Rect};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    future::Future,
    io::{self, stdout},
    time::{Duration, Instant},
};
const TIME: Duration = Duration::from_secs(5);
const GIT_REFRESH: Duration = Duration::from_secs(10);
const TOAST: Duration = Duration::from_secs(4);
struct Surface {
    ended: bool,
    retry_at: Instant,
    address: String,
    stream: Option<Stream>,
    repaint: crate::connection::Repaint,
    vt: Vt,
    /// The rows last put on the host, to send only rows that changed since.
    painted: Vec<std::sync::Arc<Vec<crate::vt::Cell>>>,
    copy: Option<Copy>,
    wanted: (u16, u16),
    error: Option<String>,
}
/// A terminal pane to keep attached; `shown` is the size it shows at, or None while hidden.
struct SurfaceTarget {
    pane: String,
    address: String,
    shown: Option<(u16, u16)>,
}
struct Click {
    at: Instant,
    pane: String,
    col: u16,
    row: u16,
    count: u8,
}
impl Click {
    /// Herdr's double-click window: the same pane within 350 ms and one cell.
    fn repeats(&self, pane: &str, col: u16, row: u16, at: Instant) -> bool {
        self.pane == pane
            && at.duration_since(self.at) <= Duration::from_millis(350)
            && self.row.abs_diff(row) <= 1
            && self.col.abs_diff(col) <= 1
    }
}
struct ResizeDrag {
    splitter: layout::Splitter,
    origin: u16,
    value: f64,
}
enum Pressed {
    Workspace(String),
    Tab(String),
}
enum Drag {
    Splitter(ResizeDrag),
    Sidebar,
    Section,
    Select(String),
    /// A double- or triple-click selection: the word or line stays as chosen until release.
    SelectWords(String),
    /// A press on a space or tab: a click on release, or a reorder once it moves.
    Press {
        target: Pressed,
        x: u16,
        y: u16,
    },
    /// A space (with its worktree group) and where it would land: before a space, or at the end.
    Workspace {
        source: String,
        target: Option<(Option<String>, u16)>,
    },
    Tab {
        tab: String,
        insert: Option<usize>,
    },
}
struct Toast {
    text: String,
    error: bool,
    until: Instant,
}
/// A copy-mode search being typed in the mode bar.
struct CopySearch {
    query: crate::editor::TextEditor,
}
pub struct Workspace {
    view_key: String,
    identities: BTreeMap<String, String>,
    renders: u64,
    /// Frames that sent only changed terminal rows.
    retained_renders: u64,
    /// What the host shows now; each frame sends only what differs from it.
    presented: ratatui::buffer::Buffer,
    /// The host cursor after the last frame.
    presented_cursor: Option<(u16, u16)>,
    /// When the person last typed into a terminal: its echo draws at once.
    typed_at: Option<Instant>,
    snapshot_builds: u64,
    layout_builds: u64,
    parsed_bytes: u64,
    attention: herdr_client_lib::view::Attention,
    /// The attention last written to the state file, to save only changes.
    attention_saved: String,
    listed_once: bool,
    /// The terminal of the latest alert, for Herdr's open notification target.
    notification_target: Option<String>,
    /// Scrollback text waiting to open in `$EDITOR` once the loop can hand over the host.
    pending_editor: Option<String>,
    notifications: notifications::Notifications,
    /// The last selection press, for Herdr's double-click and a triple-click.
    last_click: Option<Click>,
    /// The outer terminal title last written, from Herdr's `ui.window_title`.
    window_title: Option<String>,
    drag: Option<Drag>,
    pub session: Session,
    pub views: BTreeMap<String, View>,
    pub machine: String,
    machines: Vec<String>,
    surfaces: BTreeMap<String, Surface>,
    panes: Vec<Pane>,
    hits: Hits,
    overlay: Option<Overlay>,
    overlay_render: OverlayRender,
    mode: Mode,
    chrome: ChromeState,
    chrome_saved: preferences::Chrome,
    config: shell::Config,
    keys: Keymap,
    preferences: preferences::Preferences,
    sidebar_width: u16,
    sidebar_collapsed: bool,
    layout: shell::Layout,
    /// The host size the pane layout was computed for.
    area: Rect,
    snapshot: Snapshot,
    git: HashMap<String, GitInfo>,
    git_poller: git::Poller,
    focused: bool,
    display: Value,
    peek: Option<String>,
    previous_pane: Option<String>,
    copy_search: Option<CopySearch>,
    notice: String,
    toast: Option<Toast>,
}
impl Workspace {
    fn view(&self) -> View {
        self.views.get(&self.view_id()).cloned().unwrap_or_default()
    }
    fn view_id(&self) -> String {
        self.identities
            .get(&self.machine)
            .cloned()
            .unwrap_or_else(|| format!("unavailable:{}", self.machine))
    }
    fn view_mut(&mut self) -> &mut View {
        let id = self.view_id();
        self.views.entry(id).or_default()
    }
    fn toast(&mut self, text: impl Into<String>, error: bool) {
        let text = text.into();
        self.notice = text.clone();
        self.toast = Some(Toast {
            text,
            error,
            until: Instant::now() + TOAST,
        });
    }
    async fn refresh_identities(&mut self) -> Result<bool> {
        let machines = self.session.client.machine_list(TIME).await?;
        let identities = identities(&self.session.client, &machines);
        let mut changed = self.identities != identities;
        self.identities = identities;
        self.machines = machine_names(&machines);
        changed |= self.session.poll_availability().await?;
        Ok(changed)
    }
    fn document(&self) -> Option<herdr_client_lib::document::Document> {
        self.session.document(&self.machine).ok()
    }
    async fn close_finished(&mut self) -> bool {
        let machines = self
            .session
            .documents
            .iter()
            .filter(|(_, slot)| slot.document.is_some())
            .map(|(machine, _)| machine.clone())
            .collect::<Vec<_>>();
        let mut changed = false;
        for machine in machines {
            use herdr_client_lib::sync::WriteOutcome;
            match self.session.reconcile_finished(&machine).await {
                Ok(None) => {}
                Ok(Some(WriteOutcome::Written | WriteOutcome::Conflict)) => changed = true,
                Ok(Some(WriteOutcome::Unavailable(error))) => {
                    self.toast(format!("Pane cleanup failed: {error}"), true);
                    changed = true;
                }
                Err(error) => {
                    self.toast(format!("Pane cleanup failed: {error}"), true);
                    changed = true;
                }
            }
        }
        if changed {
            self.reconcile();
        }
        changed
    }
    async fn recover_placements(&mut self) -> bool {
        let outcomes = self.session.reconcile_placements().await;
        if outcomes.is_empty() {
            return false;
        }
        let errors = outcomes
            .iter()
            .filter_map(|outcome| {
                use herdr_client_lib::sync::WriteOutcome;
                let error = match &outcome.result {
                    Ok(WriteOutcome::Written) => return None,
                    Ok(WriteOutcome::Conflict) => "workspace changed during recovery".into(),
                    Ok(WriteOutcome::Unavailable(error)) => error.clone(),
                    Err(error) => error.to_string(),
                };
                Some(format!("{}: {error}", outcome.terminal))
            })
            .collect::<Vec<_>>();
        if !errors.is_empty() {
            self.toast(format!("Recovery failed: {}", errors.join("; ")), true);
        }
        self.reconcile();
        true
    }

    fn reconcile(&mut self) {
        let visible: HashSet<String> = self
            .panes
            .iter()
            .filter_map(|pane| {
                let local = pane.terminal.as_ref()?;
                Some(if pane.id.starts_with('@') {
                    local.clone()
                } else {
                    qualify(local, &self.machine)
                })
            })
            .collect();
        let placed: HashSet<String> = self
            .session
            .documents
            .iter()
            .filter_map(|(machine, slot)| Some(model::places(slot.document.as_ref()?, machine)))
            .flat_map(|places| places.into_keys())
            .collect();
        let mut alerts = Vec::new();
        let mut looked_at = Vec::new();
        for (identity, record) in &self.session.terminals {
            let word = record.word().map(str::to_owned);
            let app = record
                .status
                .as_ref()
                .map(|s| s.app.clone())
                .filter(|s| !s.is_empty());
            let looking = self.focused && visible.contains(&record.address);
            let sub_agent = record.parent.as_deref().is_some_and(|p| !p.is_empty());
            let alert = self.attention.alert_for(
                identity,
                placed.contains(&record.address),
                sub_agent,
                word.as_deref(),
                looking,
            );
            // Changes already made before this viewer started aren't news.
            if self.listed_once
                && let Some(alert) = alert
            {
                alerts.push((identity.clone(), record.address.clone(), alert));
            }
            self.attention.set_word(identity, word, app, looking);
            if looking {
                looked_at.push(identity.clone());
            }
        }
        self.listed_once = true;
        self.attention.mark_seen(&looked_at);
        // Keep what this viewer saw on machines whose terminals are not loaded now.
        let loaded: HashSet<&String> = self
            .identities
            .iter()
            .filter(|(machine, _)| self.session.document_ref(machine).is_ok())
            .map(|(_, core)| core)
            .collect();
        let terminals = &self.session.terminals;
        self.attention
            .retain(|id| terminals.contains_key(id) || !loaded.contains(&id.core));
        let now = Instant::now();
        for (identity, address, alert) in alerts {
            self.notify(identity, &address, alert, now);
        }
        let id = self.view_id();
        if let Ok(doc) = self.session.document_ref(&self.machine) {
            let view = self.views.entry(id).or_default();
            view.reconcile(doc);
            if view.focused.is_none()
                && let Some(pane) = doc.value()["spaces"].as_array().and_then(|s| {
                    s.iter().find_map(|s| {
                        s["tabs"]
                            .as_array()
                            .and_then(|t| t.iter().find_map(|t| layout::first(&t["root"])))
                    })
                })
            {
                view.focus(doc, pane);
            }
        }
    }
    fn focus(&mut self, id: &str) {
        if let Some(doc) = self.document() {
            let before = self.view().focused;
            let view = self.view_mut();
            view.focus(&doc, id);
            // Zoom follows focus inside a zoomed tab, as in Herdr.
            if view.zoomed.is_some() {
                let focused = view.focused.clone();
                view.zoom(focused.as_deref());
            }
            if before.as_deref() != Some(id) && before.is_some() {
                self.previous_pane = before.clone();
            }
            if self.peek.take().is_some() || before.as_deref() != Some(id) {
                self.chrome.reveal_focused_tab = true;
                self.chrome.reveal_focused_workspace = true;
            }
        }
    }
    fn focus_agent(&mut self, address: &str) {
        let placed = self.session.documents.keys().find_map(|machine| {
            let doc = self.session.document_ref(machine).ok()?;
            agent_pane(doc.value(), address, machine).map(|id| (machine.clone(), id))
        });
        if let Some((machine, pane)) = placed {
            self.machine = machine;
            self.focus(&pane);
        } else {
            self.peek = Some(address.into());
        }
    }
    /// Focuses a space or tab at the pane its viewer last used there.
    fn focus_container(&mut self, id: &str) {
        if let Ok(doc) = self.session.document_ref(&self.machine)
            && let Some(pane) = self.view().remembered(doc, id)
        {
            self.focus(&pane);
        }
    }
    fn check_pane_action(&self, command: &str) -> Result<()> {
        if self.peek.is_some()
            && (command.starts_with("pane.")
                || matches!(
                    command,
                    "terminal.park" | "split.row" | "split.column" | "zoom"
                ))
        {
            return Err("This terminal has no workspace pane. Keep it in a tab first.".into());
        }
        Ok(())
    }
    fn geometry(&mut self, area: Rect) {
        self.layout_builds += 1;
        self.area = area;
        let view = self.view();
        let ui = &self.preferences.ui;
        let single_tab = ui.hide_tab_bar_when_single_tab
            && view.active_space.as_ref().is_some_and(|space| {
                self.snapshot
                    .tabs
                    .iter()
                    .filter(|t| &t.workspace_id == space)
                    .count()
                    == 1
            });
        let tab_bar = if single_tab {
            shell::TabBar::Hidden
        } else if ui.tab_bar_bottom() {
            shell::TabBar::Bottom
        } else {
            shell::TabBar::Top
        };
        self.layout = shell::layout(
            area.width,
            area.height,
            shell::Shape {
                sidebar_width: self.sidebar_width,
                collapsed: self.sidebar_collapsed,
                hidden_when_collapsed: ui.sidebar_hidden_when_collapsed(),
                min_width: ui.sidebar_min_width,
                max_width: ui.sidebar_max_width,
                tab_bar,
                mobile_width_threshold: ui.mobile_width_threshold,
            },
        );
        let body = self.layout.pane_surface;
        self.panes.clear();
        if let Some(address) = &self.peek {
            self.panes.push(Pane {
                id: format!("@{address}"),
                terminal: Some(address.clone()),
                description: "Terminal".into(),
                area: body,
                content: body,
                borders: ratatui::widgets::Borders::NONE,
                scrollbar: None,
            });
        } else if let Ok(doc) = self.session.document_ref(&self.machine)
            && let Some(root) = view
                .active_tab
                .as_deref()
                .and_then(|id| doc.node(id))
                .and_then(|t| t.get("root"))
        {
            self.panes = layout::panes(root, body, view.focused.as_deref(), view.zoomed.as_deref());
            let ui = &self.preferences.ui;
            let tab_panes = layout::navigation_ids(root).len();
            if view.zoomed.is_some() {
                // Herdr's zoomed pane: framed when borders show for the tab and outer edges draw.
                let framed = ui.pane_borders.shows_borders(tab_panes > 1) && ui.pane_outer_borders;
                for pane in &mut self.panes {
                    pane.borders = if framed {
                        ratatui::widgets::Borders::ALL
                    } else {
                        ratatui::widgets::Borders::NONE
                    };
                }
            } else {
                let rects: Vec<Rect> = self.panes.iter().map(|p| p.area).collect();
                let chrome = shell::panes::apply_chrome(
                    &rects,
                    ui.pane_borders,
                    ui.pane_gaps,
                    ui.pane_outer_borders,
                );
                for (pane, (rect, borders)) in self.panes.iter_mut().zip(chrome) {
                    pane.area = rect;
                    pane.borders = borders;
                }
            }
        }
        for pane in &mut self.panes {
            let alternate = self
                .surfaces
                .get(&pane.id)
                .is_some_and(|s| s.vt.alt_screen());
            let (content, scrollbar) = shell::panes::terminal_rect(
                shell::panes::inner(pane.area, pane.borders),
                alternate || !self.preferences.ui.pane_scrollbars,
            );
            pane.content = content;
            pane.scrollbar = scrollbar;
        }
    }
    fn refresh_snapshot(&mut self) {
        self.snapshot_builds += 1;
        let doc = self.session.document_ref(&self.machine).ok();
        let view = self.view();
        let home = if self.machine.is_empty() {
            std::env::var("HOME").unwrap_or_default()
        } else {
            String::new()
        };
        self.snapshot = snapshot::build(&snapshot::Sources {
            document: doc.as_ref().map(|d| d.value()),
            view: &view,
            machine: &self.machine,
            terminals: &self.session.terminals,
            attention: &self.attention,
            home: &home,
            git: &self.git,
            peek: self.peek.as_deref(),
        });
    }
    /// Asks git about each space's folder on its machine, at most every few seconds per folder.
    fn refresh_git(&mut self) -> bool {
        if self.session.document_ref(&self.machine).is_err() {
            return false;
        }
        let folders: HashSet<String> = self
            .snapshot
            .workspaces
            .iter()
            .map(|w| w.cwd.clone())
            .filter(|cwd| !cwd.is_empty())
            .collect();
        self.git_poller
            .refresh(&self.machine, &self.session.client, folders, &mut self.git)
    }
    fn invalidate_git(&mut self) {
        self.git_poller.invalidate();
    }
    /// Every terminal pane on this machine, visible ones first with the size they show at.
    /// Hidden panes stay attached and keep parsing, as Herdr keeps every pane live,
    /// so showing one again needs no repaint and core keeps its size.
    fn surface_targets(&self) -> Vec<SurfaceTarget> {
        let mut targets: Vec<_> = self
            .panes
            .iter()
            .filter(|pane| !pane.content.is_empty())
            .filter_map(|pane| {
                let local = pane.terminal.as_ref()?;
                let address = if pane.id.starts_with('@') {
                    local.clone()
                } else {
                    qualify(local, &self.machine)
                };
                let wanted = (pane.content.width.max(1), pane.content.height.max(1));
                Some(SurfaceTarget {
                    pane: pane.id.clone(),
                    address,
                    shown: Some(wanted),
                })
            })
            .collect();
        if let Ok(document) = self.session.document_ref(&self.machine) {
            let hidden: Vec<_> = model::places(document, &self.machine)
                .into_iter()
                .filter(|(_, place)| !self.panes.iter().any(|pane| pane.id == place.pane))
                .map(|(address, place)| SurfaceTarget {
                    pane: place.pane,
                    address,
                    shown: None,
                })
                .collect();
            targets.extend(hidden);
        }
        targets
    }
    async fn sync_surfaces(&mut self, activate: bool) -> Result<()> {
        let targets = self.surface_targets();
        self.surfaces
            .retain(|id, _| targets.iter().any(|target| &target.pane == id));
        for SurfaceTarget {
            pane: id,
            address,
            shown,
        } in targets
        {
            let wanted = shown
                .or_else(|| self.surfaces.get(&id).map(|surface| surface.wanted))
                .or_else(|| {
                    self.session
                        .terminals
                        .values()
                        .find(|record| record.address == address)
                        .map(|record| crate::connection::size(record, (80, 24)))
                })
                .unwrap_or((80, 24));
            let surface = self.surfaces.entry(id).or_insert_with(|| Surface {
                ended: false,
                retry_at: Instant::now(),
                address: address.clone(),
                stream: None,
                repaint: Default::default(),
                vt: Vt::new(wanted.0, wanted.1),
                painted: Vec::new(),
                copy: None,
                wanted,
                error: None,
            });
            if surface.address != address {
                surface.address = address.clone();
                surface.ended = false;
                surface.stream = None;
                surface.copy = None;
            }
            if surface.stream.is_none() {
                if surface.ended || Instant::now() < surface.retry_at {
                    continue;
                }
                surface.retry_at = Instant::now() + Duration::from_secs(2);
                let finished = self
                    .session
                    .terminals
                    .values()
                    .any(|record| record.address == address && record.state == "finished");
                if finished {
                    match self.session.client.terminal_read(&address, 0, TIME).await {
                        Ok(result) => {
                            surface.vt = Vt::new(wanted.0, wanted.1);
                            surface
                                .vt
                                .write(result.text.replace('\n', "\r\n").as_bytes());
                            surface.ended = true;
                            surface.error = None;
                        }
                        Err(error) => surface.error = Some(error.to_string()),
                    }
                    continue;
                }
                match self
                    .session
                    .client
                    .attach(
                        &crate::connection::attach_params(&address, wanted, true, &self.display),
                        TIME,
                    )
                    .await
                {
                    Ok((record, stream)) => {
                        let size = crate::connection::size(&record, wanted);
                        surface.vt = Vt::new(size.0, size.1);
                        surface.stream = Some(stream);
                        surface.repaint.start();
                        surface.error = None;
                        surface.wanted = wanted;
                    }
                    Err(error) => {
                        surface.error = Some(error.to_string());
                        continue;
                    }
                }
            }
            if shown.is_some() && (surface.wanted != wanted || activate) {
                surface.wanted = wanted;
                if self.focused
                    && let Some(stream) = &surface.stream
                {
                    stream.resize(wanted.0, wanted.1).await?;
                }
            }
        }
        Ok(())
    }
    /// When drawing stops waiting for visible panes' attach repaints, while one is arriving.
    fn repaint_pending(&self) -> Option<Instant> {
        self.panes
            .iter()
            .filter_map(|pane| self.surfaces.get(&pane.id)?.repaint.pending_until())
            .min()
    }
    fn active_id(&self) -> Option<String> {
        self.peek
            .as_ref()
            .map(|a| format!("@{a}"))
            .or(self.view().focused)
    }
    fn active_terminal(&self) -> Option<String> {
        if let Some(address) = &self.peek {
            return Some(address.clone());
        }
        let view = self.view();
        let document = self.session.document_ref(&self.machine).ok()?;
        let pane = document.node(view.focused.as_deref()?)?;
        let terminal = pane["content"]["terminal"].as_str()?;
        Some(qualify(terminal, &self.machine))
    }
    /// Runs a shared or private command and reports failures as a toast.
    /// Herdr opens a workspace where it starts when the session has none. A
    /// never-written document is a new session; one emptied by closing stays empty.
    async fn seed_startup_space(&mut self) {
        let fresh = self.machine.is_empty()
            && self.session.documents.get("").is_some_and(|slot| {
                slot.version == 0
                    && slot
                        .document
                        .as_ref()
                        .is_some_and(|d| d.value()["spaces"].as_array().is_some_and(Vec::is_empty))
            });
        if fresh && let Ok(cwd) = std::env::current_dir() {
            self.run("space.new", json!({"folder":cwd})).await;
        }
    }
    async fn run(&mut self, command: &str, args: Value) -> Option<Value> {
        match self.command(command, args).await {
            Ok(value) => Some(value),
            Err(error) => {
                self.toast(error.to_string(), true);
                None
            }
        }
    }
    async fn command(&mut self, command: &str, mut args: Value) -> Result<Value> {
        if matches!(command, "pane.focus" | "tab.focus" | "space.focus") {
            let id = args["id"].as_str().ok_or("id is required")?;
            let doc = self.session.document_ref(&self.machine)?;
            doc.node(id).ok_or("Target unavailable")?;
            let pane = if command == "pane.focus" {
                Some(id.to_owned())
            } else {
                self.view().remembered(doc, id)
            }
            .ok_or("Target has no pane")?;
            self.focus(&pane);
            return Ok(json!({"ok":true}));
        }
        if matches!(command, "agent.focus" | "terminal.focus") {
            let address = args["terminal"].as_str().ok_or("terminal is required")?;
            if !self
                .session
                .terminals
                .values()
                .any(|r| r.address == address)
            {
                return Err("Terminal unavailable".into());
            }
            self.focus_agent(address);
            return Ok(json!({"ok":true}));
        }
        if command == "machine" {
            self.machine = args["machine"].as_str().unwrap_or("").into();
            if !self.session.documents.contains_key(&self.machine) {
                self.session.refresh(&self.machine).await;
            }
            self.peek = None;
            self.invalidate_git();
            self.reconcile();
            return Ok(json!({"ok":true}));
        }
        self.check_pane_action(command)?;
        if command == "space.new"
            && args["folder"].is_null()
            && let Some(address) = self.active_terminal()
            && address.split_once('/').map_or("", |(machine, _)| machine) == self.machine
        {
            let terminal = self.session.client.terminal_get(&address, TIME).await?;
            if let Some(folder) = terminal.folder() {
                args["folder"] = json!(folder);
            }
        }
        let mut view = self.view();
        let result =
            actions::execute(&mut self.session, &self.machine, &mut view, command, args).await?;
        self.views.insert(self.view_id(), view);
        self.reconcile();
        self.chrome.reveal_focused_tab = true;
        self.chrome.reveal_focused_workspace = true;
        if result["ok"] == false {
            let message = result["error"]
                .as_str()
                .or_else(|| result["workspace"]["error"].as_str())
                .or_else(|| result["failure"]["error"].as_str())
                .or_else(|| result["git"]["stderr"].as_str().filter(|s| !s.is_empty()))
                .map_or_else(|| format!("{command} did not complete"), str::to_owned);
            return Err(message.trim().to_owned().into());
        }
        Ok(result)
    }
    fn notification(&mut self, notification: &Notification) {
        let event = match notification {
            Notification::Event(event) => Some(event),
            _ => None,
        };
        let address = event.and_then(|e| e.terminal.as_deref());
        for surface in self.surfaces.values_mut() {
            if address == Some(&surface.address) {
                match event.map(|e| e.kind.as_str()) {
                    Some("size") => {
                        let data = &event.unwrap().data;
                        let fallback = surface.wanted;
                        let dimension = |key: &str, fallback: u16| {
                            data[key]
                                .as_u64()
                                .map(|n| n.clamp(1, 65535) as u16)
                                .unwrap_or(fallback)
                        };
                        surface
                            .vt
                            .resize(dimension("cols", fallback.0), dimension("rows", fallback.1));
                    }
                    Some("restarted" | "lost") => {
                        surface.stream = None;
                        surface.ended = false;
                    }
                    Some("finished") => surface.ended = true,
                    _ => {}
                }
            } else if matches!(notification, Notification::Missed(_)) {
                surface.stream = None;
            }
        }
        if event.is_some_and(|e| e.kind == "turn") {
            self.invalidate_git();
        }
        self.reconcile();
    }
    fn save_attention(&mut self) {
        let Ok(text) = serde_json::to_string(&self.attention) else {
            return;
        };
        if text != self.attention_saved {
            match preferences::save_attention(&self.view_key, &self.attention) {
                Ok(()) => self.attention_saved = text,
                Err(error) => self.toast(error.to_string(), true),
            }
        }
    }
    fn save_chrome(&mut self) {
        self.chrome_saved.sidebar_width = Some(self.sidebar_width);
        self.chrome_saved.sidebar_collapsed = self.sidebar_collapsed;
        self.chrome_saved.sidebar_section_split = self.chrome.section_split;
        self.chrome_saved.agent_panel_sort = Some(
            match self.config.sort {
                shell::Sort::Spaces => "spaces",
                shell::Sort::Priority => "priority",
            }
            .into(),
        );
        self.chrome_saved.collapsed_groups = self.chrome.collapsed_groups.iter().cloned().collect();
        if let Err(error) = preferences::save_chrome(&self.view_key, &self.chrome_saved) {
            self.toast(error.to_string(), true);
        }
    }
    fn reload_config(&mut self) -> Result<()> {
        let capture = self.preferences.ui.mouse_capture;
        self.preferences = preferences::load()?;
        if capture != self.preferences.ui.mouse_capture {
            crate::MOUSE_CAPTURE.store(
                self.preferences.ui.mouse_capture,
                std::sync::atomic::Ordering::Relaxed,
            );
            if self.preferences.ui.mouse_capture {
                crossterm::execute!(io::stdout(), crossterm::event::EnableMouseCapture)?;
            } else {
                crossterm::execute!(io::stdout(), crossterm::event::DisableMouseCapture)?;
            }
        }
        self.window_title = None;
        let sort = self.config.sort;
        self.config = self.preferences.shell();
        if self.chrome_saved.agent_panel_sort.is_some() {
            self.config.sort = sort;
        }
        self.keys = self.preferences.keymap();
        Ok(())
    }
}

/// Branch, checkout, and worktree identity for one folder, read with argv-based git.
async fn git_info(git: &herdr_client_lib::worktrees::Git, cwd: &str) -> Option<GitInfo> {
    let output = git
        .run(
            cwd,
            &[
                "rev-parse",
                "--path-format=absolute",
                "--show-toplevel",
                "--git-common-dir",
                "--absolute-git-dir",
                "--abbrev-ref",
                "HEAD",
            ],
        )
        .await
        .ok()?;
    if output.code != Some(0) {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let lines: Vec<&str> = text.lines().collect();
    let [top, common, dir, head] = lines.as_slice() else {
        return None;
    };
    let mut info = GitInfo {
        top: (*top).into(),
        branch: (*head != "HEAD").then(|| (*head).to_owned()),
        common_dir: common.trim_end_matches('/').into(),
        linked: dir.trim_end_matches('/') != common.trim_end_matches('/'),
        ahead_behind: None,
    };
    if info.branch.is_some()
        && let Ok(counts) = git
            .run(
                cwd,
                &["rev-list", "--left-right", "--count", "HEAD...@{upstream}"],
            )
            .await
        && counts.code == Some(0)
    {
        let text = String::from_utf8_lossy(&counts.stdout);
        let mut numbers = text.split_whitespace().filter_map(|n| n.parse().ok());
        if let (Some(ahead), Some(behind)) = (numbers.next(), numbers.next()) {
            info.ahead_behind = Some((ahead, behind));
        }
    }
    Some(info)
}

fn agent_pane(node: &Value, address: &str, machine: &str) -> Option<String> {
    if node["type"] == "block"
        && node["content"]["terminal"]
            .as_str()
            .is_some_and(|a| qualify(a, machine) == address)
    {
        return node["id"].as_str().map(str::to_owned);
    }
    for key in ["spaces", "tabs", "children"] {
        if let Some(found) = node[key]
            .as_array()
            .and_then(|items| items.iter().find_map(|n| agent_pane(n, address, machine)))
        {
            return Some(found);
        }
    }
    node.get("root")
        .and_then(|n| agent_pane(n, address, machine))
}

fn identities(
    client: &herdr_client_lib::client::Client,
    machines: &MachineList,
) -> BTreeMap<String, String> {
    let mut result = BTreeMap::new();
    if !client.core_id().is_empty() {
        result.insert(String::new(), client.core_id().into());
    }
    for record in &machines.machines {
        result.insert(record.name.clone(), record.id.clone());
    }
    result
}
fn machine_names(machines: &MachineList) -> Vec<String> {
    let mut names = vec![String::new()];
    names.extend(machines.machines.iter().map(|m| m.name.clone()));
    names
}
/// Only these can leave a finished terminal placed; the retry tick catches anything else.
fn can_finish_pane(notification: &Notification) -> bool {
    match notification {
        Notification::Event(event) => matches!(event.kind.as_str(), "finished" | "doc"),
        _ => true,
    }
}
/// A terminal title change: no pane, sidebar, or layout shows the terminal title.
fn title_only(notification: &Notification) -> bool {
    matches!(notification, Notification::Event(event) if event.kind == "title")
}
async fn next_output(
    surfaces: &mut BTreeMap<String, Surface>,
) -> (String, std::result::Result<Option<StreamEvent>, Error>) {
    std::future::poll_fn(|cx| {
        for (id, surface) in surfaces.iter_mut() {
            if let Some(stream) = &mut surface.stream
                && let std::task::Poll::Ready(event) = stream.poll_next(cx)
            {
                return std::task::Poll::Ready((id.clone(), event));
            }
        }
        std::task::Poll::Pending
    })
    .await
}
pub async fn run(
    config: HcConfig,
    initial_machine: String,
    instance: Option<String>,
    display: Value,
    mut interrupt: impl Future<Output = io::Result<()>> + Unpin,
) -> Result<()> {
    let instance = instance.unwrap_or_else(|| format!("tui-{}", std::process::id()));
    let mut endpoint = crate::control::Endpoint::bind(&config.session, &instance)?;
    let view_key = format!("{}:{instance}", config.session);
    let client = config.connect(TIME).await?;
    let machine_list = client.machine_list(TIME).await?;
    let machines = machine_names(&machine_list);
    let identities = identities(&client, &machine_list);
    let session = Session::connect(client, machines.clone(), TIME).await?;
    let preferences = preferences::load()?;
    let chrome_saved = preferences::read_chrome(&view_key);
    let mut shell_config = preferences.shell();
    if let Some(sort) = &chrome_saved.agent_panel_sort {
        shell_config.sort = if sort == "priority" {
            shell::Sort::Priority
        } else {
            shell::Sort::Spaces
        };
    }
    let mut app = Workspace {
        view_key: view_key.clone(),
        identities,
        renders: 0,
        retained_renders: 0,
        presented: Default::default(),
        presented_cursor: None,
        typed_at: None,
        snapshot_builds: 0,
        layout_builds: 0,
        parsed_bytes: 0,
        attention: {
            let mut attention = herdr_client_lib::view::Attention::default();
            attention.restore(&preferences::read_attention(&view_key));
            attention
        },
        attention_saved: String::new(),
        listed_once: false,
        notification_target: None,
        pending_editor: None,
        notifications: Default::default(),
        last_click: None,
        window_title: None,
        drag: None,
        session,
        views: preferences::read_views(&view_key),
        machine: initial_machine,
        machines,
        surfaces: BTreeMap::new(),
        panes: vec![],
        hits: Hits::default(),
        overlay: None,
        overlay_render: OverlayRender::default(),
        mode: Mode::Terminal,
        chrome: ChromeState {
            section_split: chrome_saved.sidebar_section_split,
            collapsed_groups: chrome_saved.collapsed_groups.iter().cloned().collect(),
            reveal_focused_tab: true,
            reveal_focused_workspace: true,
            ..ChromeState::default()
        },
        sidebar_width: chrome_saved
            .sidebar_width
            .unwrap_or(preferences.sidebar_width),
        sidebar_collapsed: chrome_saved.sidebar_collapsed || preferences.ui.sidebar_start_collapsed,
        chrome_saved,
        config: shell_config,
        keys: preferences.keymap(),
        preferences,
        layout: shell::Layout::default(),
        area: Rect::default(),
        snapshot: Snapshot::default(),
        git: HashMap::new(),
        git_poller: git::Poller::default(),
        focused: true,
        display,
        peek: None,
        previous_pane: None,
        copy_search: None,
        notice: format!("Instance {instance}"),
        toast: None,
    };
    if !app.session.documents.contains_key(&app.machine) {
        app.session.refresh(&app.machine).await;
    }
    app.close_finished().await;
    app.recover_placements().await;
    app.seed_startup_space().await;
    app.reconcile();
    app.refresh_snapshot();
    app.refresh_git();
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        crate::restore();
        previous(info);
    }));
    crate::MOUSE_CAPTURE.store(
        app.preferences.ui.mouse_capture,
        std::sync::atomic::Ordering::Relaxed,
    );
    let _host = Host::enter()?;
    let mut screen = Terminal::new(CrosstermBackend::new(stdout()))?;
    let mut events = EventStream::new();
    let mut dirty = true;
    // Only visible terminals printed since the last frame.
    let mut output_due = false;
    // Core events changed what the frame shows; they share the output clock.
    let mut event_due = false;
    let mut activate_layout = true;
    let mut snapshot_due = false;
    let mut layout_due = true;
    let mut render_due = true;
    let mut redraw = tokio::time::interval(Duration::from_millis(16));
    redraw.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    // herdr-app draws output on a shared ~30 Hz clock so panes printing out of step share frames.
    let mut output_clock = tokio::time::interval(Duration::from_millis(33));
    output_clock.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut connected = true;
    let mut retry = tokio::time::interval(Duration::from_secs(2));
    retry.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut notify = tokio::time::interval(Duration::from_millis(100));
    notify.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        if snapshot_due {
            app.refresh_snapshot();
            if connected {
                dirty |= app.refresh_git();
            }
            snapshot_due = false;
        }
        if (dirty || output_due || event_due) && render_due {
            let area = screen.size()?;
            let area = Rect::new(0, 0, area.width, area.height);
            if layout_due || activate_layout || area != app.area {
                app.geometry(area);
                if connected && let Err(e) = app.sync_surfaces(activate_layout).await {
                    app.toast(e.to_string(), true);
                }
            }
            activate_layout = false;
            layout_due = false;
            if app.repaint_pending().is_none() {
                if dirty || event_due || !app.draw_output(&mut screen)? {
                    let resized = app.draw(&mut screen)?;
                    dirty = resized;
                    activate_layout |= resized;
                    layout_due |= resized;
                }
                output_due = false;
                event_due = false;
                render_due = false;
            }
        }
        let repaint = app.repaint_pending();
        let typing = app
            .typed_at
            .is_some_and(|at| at.elapsed() < Duration::from_millis(200));
        enum Next {
            Host(Option<io::Result<Event>>),
            Output(String, std::result::Result<Option<StreamEvent>, Error>),
            Update(std::result::Result<Notification, Error>),
            Retry,
            Notify,
            Paint,
            Control(crate::control::Request),
            Git(git::Update),
        }
        let next = tokio::select! {
        _=&mut interrupt=>break,
        event=events.next()=>Next::Host(event),
        Some(request)=endpoint.requests.recv()=>Next::Control(request),
        output=next_output(&mut app.surfaces)=>Next::Output(output.0,output.1),
        update=app.git_poller.next(),if connected=>Next::Git(update),
        update=app.session.next(),if connected=>Next::Update(update),
        _=retry.tick()=>Next::Retry,
        _=notify.tick(),if app.notifications.active()=>Next::Notify,
        _=redraw.tick(),if dirty||output_due&&typing=>Next::Paint,
        _=output_clock.tick(),if output_due&&!typing||event_due=>Next::Paint,
        _=tokio::time::sleep_until(repaint.unwrap_or_else(Instant::now).into()),if repaint.is_some()=>Next::Paint,
        };
        match &next {
            Next::Update(Ok(notification)) if title_only(notification) => {}
            Next::Host(_) | Next::Update(_) | Next::Retry => {
                snapshot_due = true;
                layout_due = true;
            }
            Next::Control(request) if request.command != "state" => {
                snapshot_due = true;
                layout_due = true;
            }
            Next::Git(_) => snapshot_due = true,
            _ => {}
        }
        match next {
            Next::Git(update) => {
                if app.session.document_ref(&app.machine).is_err() {
                    app.invalidate_git();
                }
                dirty |= app.git_poller.apply(update, &mut app.git);
            }
            Next::Paint => render_due = true,
            Next::Control(request) => {
                let quit = request.command == "quit";
                let result = if request.command == "state" {
                    Ok(
                        json!({"instance":instance,"session":config.session,"machine":app.machine,"view":app.view(),"connected":connected,"notice":app.notice,"document_version":app.session.documents.get(&app.machine).map(|s|s.version),"document_available":app.session.document_ref(&app.machine).is_ok(),"metrics":{"renders":app.renders,"retained_renders":app.retained_renders,"snapshot_builds":app.snapshot_builds,"layout_builds":app.layout_builds,"parsed_bytes":app.parsed_bytes,"attached":app.surfaces.values().filter(|s|s.stream.is_some()).count(),"cached":app.surfaces.len()},"panes":app.panes.iter().map(|p|json!({"id":p.id,"terminal":p.terminal})).collect::<Vec<_>>()}),
                    )
                } else if quit {
                    Ok(json!({"ok":true}))
                } else {
                    let result = match request.command.as_str() {
                        "zoom" => {
                            app.toggle_zoom();
                            Ok(())
                        }
                        "pane.focus" | "tab.focus" | "space.focus" | "agent.focus"
                        | "terminal.focus" | "machine" => app
                            .command(&request.command, request.args)
                            .await
                            .map(|_| ()),
                        _ => Err(
                            "Only private view commands are accepted; use do for shared operations"
                                .into(),
                        ),
                    };
                    result.map(|()| json!({"ok":true,"view":app.view()}))
                };
                let _ = request.reply.send(match result {
                    Ok(value) => value,
                    Err(e) => json!({"error":e.to_string()}),
                });
                dirty |= request.command != "state";
                activate_layout |= request.command != "state";
                if quit {
                    let _ = tokio::time::timeout(Duration::from_secs(2), request.delivered).await;
                    break;
                }
            }
            Next::Host(event) => {
                let Some(event) = event.transpose()? else {
                    break;
                };
                let old = app.active_id();
                activate_layout |= matches!(event, Event::FocusGained | Event::Resize(_, _));
                let activate =
                    matches!(event, Event::Mouse(_) | Event::Key(_) | Event::FocusGained);
                match app.host(event).await {
                    Ok(true) => break,
                    Ok(false) => {}
                    Err(e) => app.toast(e.to_string(), true),
                }
                if let Some(text) = app.pending_editor.take() {
                    // The editor reads the keyboard; stop reading host events meanwhile.
                    drop(events);
                    let edited = Host::suspend(|| crate::scrollback::edit(&text))?;
                    events = EventStream::new();
                    screen.clear()?;
                    app.presented = Default::default();
                    app.presented_cursor = None;
                    if let Err(error) = edited {
                        app.toast(error.to_string(), true);
                    }
                }
                if old != app.active_id() && activate {
                    let size = screen.size()?;
                    app.geometry(Rect::new(0, 0, size.width, size.height));
                    if let Err(e) = app.sync_surfaces(true).await {
                        app.toast(e.to_string(), true);
                    }
                    if let Err(e) = app.activate().await {
                        app.toast(e.to_string(), true);
                    }
                }
                dirty = true;
            }
            Next::Output(id, Ok(Some(StreamEvent::Data(bytes)))) => {
                let shown = app.panes.iter().any(|pane| pane.id == id);
                if let Some(surface) = app.surfaces.get_mut(&id) {
                    app.parsed_bytes += bytes.len() as u64;
                    let alternate = surface.vt.alt_screen();
                    surface.vt.write(&bytes);
                    surface.repaint.received(bytes.len());
                    let switched = shown && alternate != surface.vt.alt_screen();
                    activate_layout |= switched;
                    dirty |= switched;
                    if let Some(stream) = &surface.stream
                        && let Err(e) = stream.consumed(bytes.len() as u32).await
                    {
                        surface.error = Some(e.to_string());
                        surface.stream = None;
                    }
                }
                // Hidden panes parse to stay current but draw nothing until shown.
                output_due |= shown;
            }
            Next::Output(id, outcome) => {
                if let Some(surface) = app.surfaces.get_mut(&id) {
                    surface.stream = None;
                    surface.repaint.end();
                    surface.ended |= matches!(outcome, Ok(Some(StreamEvent::Closed(ref value))) if value["reason"] == "exited");
                    surface.error = Some(
                        if surface.ended {
                            "Terminal finished"
                        } else {
                            "Terminal stream ended; waiting for core"
                        }
                        .into(),
                    );
                }
                dirty = true;
            }
            // Agents animate their titles; only the host window title can show them.
            Next::Update(Ok(notification)) if title_only(&notification) => {
                app.update_window_title()?;
            }
            Next::Update(Ok(notification)) => {
                if matches!(notification, Notification::Missed(_))
                    && let Err(e) = app.refresh_identities().await
                {
                    app.toast(e.to_string(), true);
                }
                if !matches!(notification, Notification::Heartbeat { .. }) {
                    if can_finish_pane(&notification) {
                        app.close_finished().await;
                    }
                    app.notification(&notification);
                    event_due = true;
                }
            }
            Next::Update(Err(e)) => {
                app.toast(format!("Disconnected: {e}; reconnecting"), true);
                connected = false;
                for surface in app.surfaces.values_mut() {
                    surface.stream = None;
                }
                dirty = true;
            }
            Next::Notify => dirty |= app.tick_notifications(Instant::now()),
            Next::Retry => {
                if app
                    .toast
                    .as_ref()
                    .is_some_and(|t| Instant::now() >= t.until)
                {
                    app.toast = None;
                    dirty = true;
                }
                if connected {
                    dirty |= app.close_finished().await;
                    dirty |= app.recover_placements().await;
                }
                app.save_attention();
                if connected && app.session.documents.keys().any(|name| !name.is_empty()) {
                    match app.refresh_identities().await {
                        Ok(changed) => {
                            dirty |= changed;
                            if changed {
                                app.reconcile();
                            }
                        }
                        Err(error) => app.toast(error.to_string(), true),
                    }
                }
                if !connected {
                    match config.connect(TIME).await {
                        Ok(client) => match app.session.reconnect(client).await {
                            Ok(()) => {
                                connected = true;
                                if let Err(e) = app.refresh_identities().await {
                                    app.toast(e.to_string(), true);
                                }
                                app.toast("Reconnected", false);
                                app.close_finished().await;
                                app.reconcile();
                                dirty = true;
                            }
                            Err(e) => app.toast(e.to_string(), true),
                        },
                        Err(e) => app.toast(e.to_string(), true),
                    }
                } else if app.panes.iter().any(|p| {
                    app.surfaces
                        .get(&p.id)
                        .is_some_and(|s| s.stream.is_none() && !s.ended)
                }) {
                    dirty = true;
                }
            }
        }
    }
    preferences::save_views(&view_key, &app.views)?;
    app.save_attention();
    app.save_chrome();
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_title_events_skip_redraws() {
        let event = |kind: &str| {
            Notification::Event(herdr_client_lib::protocol::Event {
                seq: 1,
                kind: kind.into(),
                terminal: Some("t1".into()),
                machine: None,
                data: json!({}),
            })
        };
        assert!(title_only(&event("title")));
        assert!(can_finish_pane(&event("finished")) && can_finish_pane(&event("doc")));
        assert!(!can_finish_pane(&event("status")) && !can_finish_pane(&event("labels")));
        for kind in ["status", "turn", "labels", "size", "doc", "finished"] {
            assert!(!title_only(&event(kind)), "{kind}");
        }
    }
    #[test]
    fn navigation_restores_tab_and_space_focus() {
        use herdr_client_lib::document::{Document, terminal_pane};
        let mut doc = Document::default();
        for space in ["s1", "s2"] {
            doc.create_space(space, "", "").unwrap();
        }
        for (space, tab, pane) in [("s1", "t1", "a"), ("s1", "t2", "b"), ("s2", "t3", "c")] {
            doc.create_tab(space, tab, "", terminal_pane(pane, pane))
                .unwrap();
        }
        doc.split(
            "b",
            "split",
            herdr_client_lib::document::Axis::Row,
            terminal_pane("d", "d"),
        )
        .unwrap();
        let mut view = View::default();
        for id in ["a", "d", "c"] {
            view.focus(&doc, id);
        }
        assert_eq!(view.remembered(&doc, "s1").as_deref(), Some("d"));
        assert_eq!(view.remembered(&doc, "t2").as_deref(), Some("d"));
        doc.remove("d").unwrap();
        view.reconcile(&doc);
        assert_eq!(view.remembered(&doc, "t2").as_deref(), Some("b"));
    }
    #[test]
    fn agent_target_resolves_only_placed_terminal() {
        let doc = json!({"spaces":[{"tabs":[{"root":{"type":"row","children":[
            {"type":"block","id":"p","content":{"terminal":"t1"}}
        ]}}]}]});
        assert_eq!(
            agent_pane(&doc, &qualify("t1", "remote"), "remote"),
            Some("p".into())
        );
        assert_eq!(agent_pane(&doc, "missing", "remote"), None);
    }
}
