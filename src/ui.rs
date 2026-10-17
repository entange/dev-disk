use std::collections::{BTreeSet, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use gpui::{
    AnyElement, App, ClickEvent, Context, Div, ElementId, FontWeight, Hsla, InteractiveElement, IntoElement,
    ParentElement, PathPromptOptions, Render, SharedString, Stateful, StatefulInteractiveElement, Styled, Task,
    UniformListScrollHandle, Window, div, hsla, prelude::FluentBuilder, px, relative, rgb, uniform_list,
};
use parking_lot::Mutex;

use crate::fmt;
use crate::rules::{self, Category, Classifier, NO_RULE, Risk};
use crate::scan::{self, F_DENIED, F_OTHER_VOLUME, F_SKIPPED, Kind, Node, Progress, ScanConfig, ScanOutput};
use crate::trash::{self, TrashOutcome, TrashRequest};
use crate::treemap::{self, Rect};
use crate::volumes::{self, Volume};

// ------------------------------------------------------------------ theme

const BG: u32 = 0x111113;
const SIDEBAR: u32 = 0x0c0c0e;
const SURFACE: u32 = 0x18181b;
const SURFACE_HI: u32 = 0x232327;
const HOVER: u32 = 0x1e1e22;
const BORDER: u32 = 0x2a2a30;
const HAIRLINE: u32 = 0x202024;
const TEXT: u32 = 0xececef;
const MUTED: u32 = 0x8e8e97;
const FAINT: u32 = 0x5d5d66;
const ACCENT: u32 = 0x7b8cff;
const DANGER: u32 = 0xff5f57;
const WARN: u32 = 0xf0b44c;
const OK: u32 = 0x4cc38a;

const SIDEBAR_W: f32 = 236.0;
const LIST_W: f32 = 440.0;
const HEADER_H: f32 = 56.0;
const CRUMB_H: f32 = 44.0;
const STATUS_H: f32 = 30.0;
const ROW_H: f32 = 32.0;
const DEV_ROW_H: f32 = 48.0;
const PAD: f32 = 16.0;

/// Clicks on the final "Move to Trash" button are ignored for this long
/// after the confirmation dialog opens (protects against double clicks).
const CONFIRM_ARM_DELAY: Duration = Duration::from_millis(900);

fn c(hex: u32) -> Hsla {
    rgb(hex).into()
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Lens,
    Dev,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Btn {
    Primary,
    Secondary,
    Danger,
    Ghost,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Check {
    Off,
    Partial,
    On,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum BannerKind {
    Success,
    Warn,
}

struct Banner {
    kind: BannerKind,
    text: String,
    details: Vec<String>,
}

struct ActiveScan {
    title: String,
    root: PathBuf,
    progress: Arc<Progress>,
    started: Instant,
    open_dev_tab: bool,
    _task: Task<()>,
}

struct DevGroup {
    cat: Category,
    items: Vec<usize>,
    total: u64,
}

#[derive(Clone, Copy)]
enum DevRow {
    Header(usize),
    Item(usize),
}

struct Confirm {
    opened: Instant,
    items: Vec<usize>,
    total: u64,
    caution: usize,
}

pub struct DevDisk {
    home: PathBuf,
    classifier: Arc<Classifier>,
    volumes: Vec<Volume>,
    scanning: Option<ActiveScan>,
    result: Option<ScanOutput>,
    scanned_total: u64,
    generation: u64,
    nav: Vec<u32>,
    visible: Vec<u32>,
    visible_key: Option<(Vec<u32>, u64)>,
    tab: Tab,
    hover: Option<SharedString>,
    selected: BTreeSet<usize>,
    collapsed: HashSet<Category>,
    groups: Vec<DevGroup>,
    dev_rows: Vec<DevRow>,
    confirm: Option<Confirm>,
    trashing: bool,
    banner: Option<Banner>,
    list_scroll: UniformListScrollHandle,
    dev_scroll: UniformListScrollHandle,
    _tasks: Vec<Task<()>>,
}

impl DevDisk {
    pub fn new(_cx: &mut Context<Self>) -> Self {
        let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
        let home = std::fs::canonicalize(&home).unwrap_or(home);
        Self {
            classifier: Arc::new(Classifier::new(home.clone())),
            home,
            volumes: volumes::list(),
            scanning: None,
            result: None,
            scanned_total: 0,
            generation: 0,
            nav: Vec::new(),
            visible: Vec::new(),
            visible_key: None,
            tab: Tab::Lens,
            hover: None,
            selected: BTreeSet::new(),
            collapsed: HashSet::new(),
            groups: Vec::new(),
            dev_rows: Vec::new(),
            confirm: None,
            trashing: false,
            banner: None,
            list_scroll: UniformListScrollHandle::new(),
            dev_scroll: UniformListScrollHandle::new(),
            _tasks: Vec::new(),
        }
    }

    // ------------------------------------------------------------ scanning

    fn start_scan(&mut self, cfg: ScanConfig, open_dev_tab: bool, cx: &mut Context<Self>) {
        if self.trashing {
            return;
        }
        self.cancel_scan();
        self.result = None;
        self.confirm = None;
        self.selected.clear();
        self.groups.clear();
        self.dev_rows.clear();
        self.nav.clear();
        self.visible_key = None;
        self.hover = None;
        self.banner = None;

        let progress = Arc::new(Progress::default());
        let slot: Arc<Mutex<Option<Option<ScanOutput>>>> = Arc::new(Mutex::new(None));
        let title = cfg.title.clone();
        let root = cfg.root.clone();
        {
            let progress = progress.clone();
            let slot = slot.clone();
            let classifier = self.classifier.clone();
            let spawned = std::thread::Builder::new().name("devdisk-scan".into()).spawn(move || {
                let out = scan::run(cfg, progress, classifier);
                *slot.lock() = Some(out);
            });
            if spawned.is_err() {
                self.banner = Some(Banner { kind: BannerKind::Warn, text: "Could not start the scan thread".into(), details: vec![] });
                return;
            }
        }
        let task = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_millis(80)).await;
                let done = slot.lock().take();
                let keep_going = this.update(cx, |this, cx| {
                    match done {
                        Some(out) => {
                            this.finish_scan(out, cx);
                            false
                        }
                        None => {
                            cx.notify();
                            true
                        }
                    }
                });
                if !matches!(keep_going, Ok(true)) {
                    break;
                }
            }
        });
        self.scanning = Some(ActiveScan { title, root, progress, started: Instant::now(), open_dev_tab, _task: task });
        cx.notify();
    }

    fn cancel_scan(&mut self) {
        if let Some(s) = self.scanning.take() {
            s.progress.cancel.store(true, Ordering::Relaxed);
        }
    }

    fn finish_scan(&mut self, out: Option<ScanOutput>, cx: &mut Context<Self>) {
        let open_dev = self.scanning.as_ref().map(|s| s.open_dev_tab).unwrap_or(false);
        self.scanning = None;
        if let Some(out) = out {
            self.scanned_total = out.root.size;
            self.result = Some(out);
            self.generation += 1;
            self.rebuild_dev_rows();
            if open_dev {
                self.tab = Tab::Dev;
            }
        }
        self.volumes = volumes::list();
        cx.notify();
    }

    fn scan_volume(&mut self, ix: usize, cx: &mut Context<Self>) {
        if let Some(v) = self.volumes.get(ix) {
            let cfg = volumes::config_for_volume(v);
            self.tab = Tab::Lens;
            self.start_scan(cfg, false, cx);
        }
    }

    fn scan_folder(&mut self, path: &Path, open_dev_tab: bool, cx: &mut Context<Self>) {
        match volumes::config_for_folder(path) {
            Some(cfg) => {
                if !open_dev_tab {
                    self.tab = Tab::Lens;
                }
                self.start_scan(cfg, open_dev_tab, cx)
            }
            None => {
                self.banner = Some(Banner {
                    kind: BannerKind::Warn,
                    text: format!("Cannot open {}", path.display()),
                    details: vec![],
                });
                cx.notify();
            }
        }
    }

    fn choose_folder(&mut self, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Scan".into()),
        });
        let task = cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(paths))) = rx.await {
                if let Some(p) = paths.into_iter().next() {
                    this.update(cx, |this, cx| this.scan_folder(&p, false, cx)).ok();
                }
            }
        });
        self._tasks.push(task);
    }

    fn rescan(&mut self, cx: &mut Context<Self>) {
        let Some(res) = &self.result else { return };
        let root = res.root_path.clone();
        if let Some(ix) = self.volumes.iter().position(|v| v.mount == root) {
            self.scan_volume(ix, cx);
        } else {
            self.scan_folder(&root, false, cx);
        }
    }

    // ---------------------------------------------------------- navigation

    fn current(&self) -> Option<&Node> {
        let res = self.result.as_ref()?;
        scan::node_at(&res.root, &self.nav)
    }

    fn current_path(&self) -> PathBuf {
        let Some(res) = &self.result else { return PathBuf::new() };
        let mut p = res.root_path.clone();
        let mut cur = &res.root;
        for &i in &self.nav {
            cur = &cur.children[i as usize];
            p.push(&*cur.name);
        }
        p
    }

    fn refresh_visible(&mut self) {
        let key = (self.nav.clone(), self.generation);
        if self.visible_key.as_ref() == Some(&key) {
            return;
        }
        let mut vis: Vec<u32> = Vec::new();
        if let Some(cur) = self.current() {
            vis = (0..cur.children.len() as u32).filter(|&i| !cur.children[i as usize].trashed()).collect();
            let ch = &cur.children;
            vis.sort_by(|a, b| ch[*b as usize].size.cmp(&ch[*a as usize].size));
        }
        self.visible = vis;
        self.visible_key = Some(key);
    }

    fn open_child(&mut self, child: u32, cx: &mut Context<Self>) {
        let Some(cur) = self.current() else { return };
        let Some(node) = cur.children.get(child as usize) else { return };
        if node.kind == Kind::Dir && !node.trashed() && !node.children.is_empty() {
            self.nav.push(child);
            self.hover = None;
            self.list_scroll = UniformListScrollHandle::new();
            cx.notify();
        }
    }

    fn go_to_depth(&mut self, depth: usize, cx: &mut Context<Self>) {
        if depth < self.nav.len() {
            self.nav.truncate(depth);
            self.hover = None;
            self.list_scroll = UniformListScrollHandle::new();
            cx.notify();
        }
    }

    fn display_path(&self, p: &Path) -> String {
        match p.strip_prefix(&self.home) {
            Ok(rel) if rel.as_os_str().is_empty() => "~".into(),
            Ok(rel) => format!("~/{}", rel.display()),
            Err(_) => p.display().to_string(),
        }
    }

    // ------------------------------------------------------------ dev view

    fn rebuild_dev_rows(&mut self) {
        self.groups.clear();
        self.dev_rows.clear();
        let Some(res) = &self.result else { return };
        for cat in Category::ALL {
            let items: Vec<usize> = res
                .dev_items
                .iter()
                .enumerate()
                .filter(|(_, d)| !d.trashed && rules::rule(d.rule).category == cat)
                .map(|(i, _)| i)
                .collect();
            if items.is_empty() {
                continue;
            }
            let total = items.iter().map(|&i| res.dev_items[i].size).sum();
            self.groups.push(DevGroup { cat, items, total });
        }
        self.groups.sort_by(|a, b| b.total.cmp(&a.total));
        for (gi, g) in self.groups.iter().enumerate() {
            self.dev_rows.push(DevRow::Header(gi));
            if !self.collapsed.contains(&g.cat) {
                self.dev_rows.extend(g.items.iter().map(|&i| DevRow::Item(i)));
            }
        }
        let valid: BTreeSet<usize> = self.selected.iter().copied().filter(|&i| self.selectable(i)).collect();
        self.selected = valid;
    }

    fn selectable(&self, i: usize) -> bool {
        let Some(res) = &self.result else { return false };
        match res.dev_items.get(i) {
            Some(d) => !d.trashed && rules::rule(d.rule).risk != Risk::InfoOnly,
            None => false,
        }
    }

    fn dev_total(&self) -> u64 {
        self.groups.iter().map(|g| g.total).sum()
    }

    fn selected_total(&self) -> u64 {
        let Some(res) = &self.result else { return 0 };
        self.selected.iter().map(|&i| res.dev_items[i].size).sum()
    }

    fn group_check(&self, gi: usize) -> Check {
        let g = &self.groups[gi];
        let sel: Vec<usize> = g.items.iter().copied().filter(|&i| self.selectable(i)).collect();
        if sel.is_empty() {
            return Check::Off;
        }
        let n = sel.iter().filter(|i| self.selected.contains(i)).count();
        if n == 0 {
            Check::Off
        } else if n == sel.len() {
            Check::On
        } else {
            Check::Partial
        }
    }

    fn toggle_group(&mut self, gi: usize, cx: &mut Context<Self>) {
        let check = self.group_check(gi);
        let items: Vec<usize> = self.groups[gi].items.iter().copied().filter(|&i| self.selectable(i)).collect();
        for i in items {
            if check == Check::On {
                self.selected.remove(&i);
            } else {
                self.selected.insert(i);
            }
        }
        cx.notify();
    }

    fn toggle_item(&mut self, i: usize, cx: &mut Context<Self>) {
        if !self.selectable(i) {
            return;
        }
        if !self.selected.remove(&i) {
            self.selected.insert(i);
        }
        cx.notify();
    }

    fn toggle_collapsed(&mut self, gi: usize, cx: &mut Context<Self>) {
        let cat = self.groups[gi].cat;
        if !self.collapsed.remove(&cat) {
            self.collapsed.insert(cat);
        }
        self.rebuild_dev_rows();
        cx.notify();
    }

    fn select_where(&mut self, f: impl Fn(&scan::DevItem) -> bool, cx: &mut Context<Self>) {
        let Some(res) = &self.result else { return };
        let picks: Vec<usize> = res
            .dev_items
            .iter()
            .enumerate()
            .filter(|(_, d)| !d.trashed && rules::rule(d.rule).risk == Risk::Safe && f(d))
            .map(|(i, _)| i)
            .collect();
        self.selected.extend(picks);
        cx.notify();
    }

    fn open_confirm(&mut self, cx: &mut Context<Self>) {
        if self.scanning.is_some() || self.trashing || self.selected.is_empty() {
            return;
        }
        let Some(res) = &self.result else { return };
        let items: Vec<usize> = self.selected.iter().copied().filter(|&i| self.selectable(i)).collect();
        if items.is_empty() {
            return;
        }
        let total = items.iter().map(|&i| res.dev_items[i].size).sum();
        let caution = items.iter().filter(|&&i| rules::rule(res.dev_items[i].rule).risk == Risk::Caution).count();
        self.confirm = Some(Confirm { opened: Instant::now(), items, total, caution });
        // Re-render once the confirm button becomes armed.
        let task = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(CONFIRM_ARM_DELAY + Duration::from_millis(30)).await;
            this.update(cx, |_, cx| cx.notify()).ok();
        });
        self._tasks.push(task);
        cx.notify();
    }

    fn run_trash(&mut self, cx: &mut Context<Self>) {
        let Some(confirm) = &self.confirm else { return };
        if self.trashing || self.scanning.is_some() || confirm.opened.elapsed() < CONFIRM_ARM_DELAY {
            return;
        }
        let Some(res) = &self.result else { return };
        let reqs: Vec<TrashRequest> = confirm
            .items
            .iter()
            .copied()
            .filter(|&i| self.selectable(i))
            .map(|i| {
                let d = &res.dev_items[i];
                TrashRequest { path: d.path.clone(), rule: d.rule, size: d.size, item: i }
            })
            .collect();
        if reqs.is_empty() {
            self.confirm = None;
            cx.notify();
            return;
        }
        self.trashing = true;
        let classifier = self.classifier.clone();
        let task = cx.spawn(async move |this, cx| {
            let outcomes = cx
                .background_executor()
                .spawn(async move { reqs.iter().map(|r| trash::trash_one(r, &classifier)).collect::<Vec<_>>() })
                .await;
            this.update(cx, |this, cx| this.finish_trash(outcomes, cx)).ok();
        });
        self._tasks.push(task);
        cx.notify();
    }

    fn finish_trash(&mut self, outcomes: Vec<TrashOutcome>, cx: &mut Context<Self>) {
        self.trashing = false;
        self.confirm = None;
        let mut moved = 0usize;
        let mut moved_bytes = 0u64;
        let mut refused = Vec::new();
        if let Some(res) = &mut self.result {
            for o in outcomes {
                match o {
                    TrashOutcome::Moved { item, size } => {
                        moved += 1;
                        moved_bytes += size;
                        if let Some(d) = res.dev_items.get_mut(item) {
                            d.trashed = true;
                            let idx = d.idx.clone();
                            scan::mark_trashed(&mut res.root, &idx);
                        }
                    }
                    TrashOutcome::Refused { path, reason } => {
                        refused.push(format!("{} — {}", path.display(), reason));
                    }
                }
            }
        }
        self.selected.clear();
        self.generation += 1;
        self.rebuild_dev_rows();
        self.volumes = volumes::list();
        let text = if moved > 0 {
            format!(
                "Moved {} item{} ({}) to the Trash. Empty the Trash in Finder to actually free the space.",
                moved,
                if moved == 1 { "" } else { "s" },
                fmt::bytes(moved_bytes)
            )
        } else {
            "Nothing was moved.".to_string()
        };
        let kind = if refused.is_empty() { BannerKind::Success } else { BannerKind::Warn };
        let mut details = Vec::new();
        if !refused.is_empty() {
            details.push(format!("{} item(s) were skipped by the safety checks:", refused.len()));
            details.extend(refused.into_iter().take(6));
        }
        self.banner = Some(Banner { kind, text, details });
        cx.notify();
    }
}

#[cfg(debug_assertions)]
impl DevDisk {
    /// Debug builds only: `DEVDISK_AUTOSCAN=<folder>` starts a (read-only) scan on launch,
    /// `DEVDISK_TAB=dev` opens the cleanup tab when it finishes. Used for UI screenshots.
    pub fn debug_autostart(&mut self, cx: &mut Context<Self>) {
        if let Some(p) = std::env::var_os("DEVDISK_AUTOSCAN") {
            let dev = std::env::var("DEVDISK_TAB").as_deref() == Ok("dev");
            self.scan_folder(Path::new(&p), dev, cx);
        }
    }
}

// --------------------------------------------------------------- widgets

fn clear() -> Hsla {
    hsla(0., 0., 0., 0.)
}

fn button(id: impl Into<ElementId>, label: impl Into<SharedString>, kind: Btn, enabled: bool) -> Stateful<Div> {
    let (bg, fg, hover) = match kind {
        Btn::Primary => (c(ACCENT), c(0xffffff), hsla(0.64, 1.0, 0.80, 1.0)),
        Btn::Secondary => (c(SURFACE_HI), c(TEXT), c(0x2c2c31)),
        Btn::Danger => (c(DANGER), c(0xffffff), hsla(0.01, 1.0, 0.72, 1.0)),
        Btn::Ghost => (clear(), c(MUTED), c(SURFACE_HI)),
    };
    let d = div()
        .id(id)
        .flex()
        .flex_shrink_0()
        .items_center()
        .justify_center()
        .gap_1()
        .h(px(28.))
        .px(px(11.))
        .rounded(px(7.))
        .bg(bg)
        .text_color(fg)
        .text_size(px(12.5))
        .font_weight(FontWeight::MEDIUM)
        .whitespace_nowrap()
        .child(label.into());
    if enabled {
        d.cursor_pointer().hover(move |s| {
            let s = s.bg(hover);
            if kind == Btn::Ghost { s.text_color(c(TEXT)) } else { s }
        })
    } else {
        d.opacity(0.35)
    }
}

fn icon_button(id: impl Into<ElementId>, glyph: &'static str, enabled: bool) -> Stateful<Div> {
    let d = div()
        .id(id)
        .flex()
        .flex_shrink_0()
        .items_center()
        .justify_center()
        .size(px(28.))
        .rounded(px(7.))
        .text_color(c(MUTED))
        .child(glyph);
    if enabled {
        d.cursor_pointer().hover(|s| s.bg(c(SURFACE_HI)).text_color(c(TEXT)))
    } else {
        d.opacity(0.3)
    }
}

fn checkbox(state: Check, enabled: bool) -> Div {
    let on = state != Check::Off;
    div()
        .flex()
        .flex_shrink_0()
        .items_center()
        .justify_center()
        .size(px(16.))
        .rounded(px(5.))
        .border_1()
        .border_color(if on { c(ACCENT) } else { c(0x45454d) })
        .bg(if on { c(ACCENT) } else { clear() })
        .text_color(c(0xffffff))
        .text_size(px(11.))
        .font_weight(FontWeight::BOLD)
        .when(!enabled, |d| d.opacity(0.2))
        .child(match state {
            Check::Off => "",
            Check::Partial => "–",
            Check::On => "✓",
        })
}

fn dot(color: Hsla, size: f32) -> Div {
    div().flex_shrink_0().size(px(size)).rounded_full().bg(color)
}

fn pill(text: impl Into<SharedString>, color: Hsla) -> Div {
    div()
        .flex()
        .flex_shrink_0()
        .items_center()
        .h(px(18.))
        .px(px(7.))
        .rounded_full()
        .text_size(px(11.))
        .font_weight(FontWeight::MEDIUM)
        .bg(color.opacity(0.13))
        .text_color(color)
        .whitespace_nowrap()
        .child(text.into())
}

fn risk_color(r: Risk) -> Hsla {
    match r {
        Risk::Safe => c(OK),
        Risk::Caution => c(WARN),
        Risk::InfoOnly => c(MUTED),
    }
}

fn usage_color(frac: f32) -> Hsla {
    if frac > 0.9 {
        c(DANGER)
    } else if frac > 0.75 {
        c(WARN)
    } else {
        c(ACCENT)
    }
}

fn bar(frac: f32, color: Hsla, height: f32) -> Div {
    div()
        .w_full()
        .h(px(height))
        .rounded_full()
        .bg(hsla(0., 0., 1., 0.07))
        .overflow_hidden()
        .child(div().h_full().rounded_full().bg(color).w(relative(frac.clamp(0.0, 1.0))))
}

fn caption(t: impl Into<SharedString>) -> Div {
    div().text_size(px(11.5)).text_color(c(MUTED)).child(t.into())
}

fn section_label(t: &'static str) -> Div {
    div()
        .px_2()
        .mt(px(18.))
        .mb(px(4.))
        .text_size(px(11.))
        .font_weight(FontWeight::MEDIUM)
        .text_color(c(FAINT))
        .child(t)
}

/// Small rounded square holding a short text glyph.
fn glyph_box(glyph: &'static str, color: Hsla) -> Div {
    div()
        .flex()
        .flex_shrink_0()
        .items_center()
        .justify_center()
        .size(px(26.))
        .rounded(px(7.))
        .bg(color.opacity(0.14))
        .text_color(color)
        .text_size(px(11.5))
        .font_weight(FontWeight::SEMIBOLD)
        .child(glyph)
}

/// Rounded square that fills from the bottom with the drive's usage.
fn gauge_box(frac: f32, color: Hsla) -> Div {
    div()
        .relative()
        .flex_shrink_0()
        .size(px(26.))
        .rounded(px(7.))
        .bg(hsla(0., 0., 1., 0.06))
        .overflow_hidden()
        .child(
            div()
                .absolute()
                .left_0()
                .bottom_0()
                .w_full()
                .h(relative(frac.clamp(0.06, 1.0)))
                .bg(color.opacity(0.45)),
        )
}

/// Tiny version of the app icon: a five-tile treemap.
fn logo_mark() -> Div {
    let tile = |color: u32| div().rounded(px(1.5)).bg(c(color));
    let g = px(1.5);
    div()
        .flex()
        .flex_shrink_0()
        .gap(g)
        .size(px(16.))
        .child(tile(0x7b85f0).w(px(8.5)).h_full())
        .child(
            div()
                .flex()
                .flex_col()
                .flex_1()
                .gap(g)
                .child(tile(0x9a78e2).h(px(7.)))
                .child(
                    div()
                        .flex()
                        .flex_1()
                        .gap(g)
                        .child(tile(0xf2a245).flex_1())
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .flex_1()
                                .gap(g)
                                .child(tile(0x45b8ab).flex_1())
                                .child(tile(0x4d5266).flex_1()),
                        ),
                ),
        )
}

fn ext_of(name: &str) -> String {
    match name.rfind('.') {
        Some(i) if i > 0 && i + 1 < name.len() => name[i + 1..].to_ascii_lowercase(),
        _ => String::new(),
    }
}

const HUE_VIDEO: f32 = 0.76;
const HUE_IMAGE: f32 = 0.40;
const HUE_AUDIO: f32 = 0.88;
const HUE_ARCHIVE: f32 = 0.14;
const HUE_MODEL: f32 = 0.98;
const HUE_DATA: f32 = 0.50;

fn file_hue(name: &str) -> Option<f32> {
    let e = ext_of(name);
    let h = match e.as_str() {
        "mp4" | "mov" | "mkv" | "avi" | "m4v" | "webm" | "prores" | "mts" => HUE_VIDEO,
        "jpg" | "jpeg" | "png" | "heic" | "gif" | "psd" | "raw" | "cr2" | "nef" | "arw" | "dng" | "tif" | "tiff"
        | "webp" => HUE_IMAGE,
        "mp3" | "wav" | "flac" | "aac" | "m4a" | "aiff" | "aif" | "ogg" => HUE_AUDIO,
        "zip" | "tar" | "gz" | "tgz" | "rar" | "7z" | "xz" | "zst" | "bz2" | "dmg" | "iso" | "pkg" | "xip" | "ipsw" => {
            HUE_ARCHIVE
        }
        "gguf" | "safetensors" | "bin" | "pt" | "pth" | "onnx" | "ckpt" | "mlmodel" | "mlpackage" | "h5" | "npz" => {
            HUE_MODEL
        }
        "db" | "sqlite" | "sqlite3" | "sql" | "csv" | "parquet" | "json" | "jsonl" | "log" | "arrow" => HUE_DATA,
        "pdf" | "doc" | "docx" | "key" | "pages" | "numbers" | "xls" | "xlsx" | "ppt" | "pptx" | "epub" => 0.30,
        "img" | "vmdk" | "qcow2" | "vdi" | "sparseimage" | "sparsebundle" => 0.55,
        _ => return None,
    };
    Some(h)
}

fn file_color(h: f32) -> Hsla {
    hsla(h, 0.42, 0.47, 1.)
}

fn folder_color() -> Hsla {
    hsla(0.63, 0.30, 0.42, 1.)
}

fn junk_color() -> Hsla {
    hsla(0.085, 0.72, 0.53, 1.)
}

/// Soft slate/indigo family for folders so the treemap reads calmly.
const FOLDER_TONES: [(f32, f32); 6] = [(0.63, 0.40), (0.58, 0.36), (0.68, 0.41), (0.55, 0.34), (0.72, 0.38), (0.61, 0.45)];

fn node_color(n: &Node, ix: usize) -> Hsla {
    if n.trashed() {
        return hsla(0., 0., 0.2, 1.);
    }
    if n.rule != NO_RULE {
        return match rules::rule(n.rule).risk {
            Risk::InfoOnly => hsla(0.08, 0.22, 0.36, 1.),
            _ => junk_color(),
        };
    }
    match n.kind {
        Kind::Dir => {
            let (h, l) = FOLDER_TONES[ix % FOLDER_TONES.len()];
            hsla(h, 0.30, l, 1.)
        }
        Kind::Others => hsla(0.65, 0.04, 0.27, 1.),
        Kind::File => match file_hue(&n.name) {
            Some(h) => file_color(h),
            None => hsla(0.65, 0.06, 0.40, 1.),
        },
    }
}

/// Colour for a tile nested inside `parent`: plain folders become lighter tints
/// of the parent so a big folder reads as one block; typed files and dev junk keep
/// their own colours.
fn nested_color(n: &Node, parent: Hsla, j: usize) -> Hsla {
    if n.rule == NO_RULE && n.kind == Kind::Dir && !n.trashed() {
        hsla(parent.h, parent.s, (parent.l + 0.07 + (j % 3) as f32 * 0.025).min(0.7), 1.)
    } else if n.kind == Kind::Others {
        hsla(parent.h, parent.s * 0.4, parent.l + 0.02, 1.)
    } else {
        node_color(n, j)
    }
}

// ----------------------------------------------------------------- render

impl Render for DevDisk {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.refresh_visible();
        let vp = window.viewport_size();
        let (vw, vh) = (f32::from(vp.width), f32::from(vp.height));

        div()
            .relative()
            .flex()
            .size_full()
            .bg(c(BG))
            .text_color(c(TEXT))
            .font_family(".SystemUIFont")
            .text_size(px(13.))
            .child(self.render_sidebar(cx))
            .child(self.render_main(vw, vh, cx))
            .when(self.confirm.is_some(), |d| d.child(self.render_confirm(cx)))
    }
}

impl DevDisk {
    fn render_sidebar(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let busy = self.trashing;
        let current_root = self.result.as_ref().map(|r| r.root_path.clone());
        let scanning_root = self.scanning.as_ref().map(|s| s.root.clone());
        let is_active = |p: &Path| current_root.as_deref() == Some(p) || scanning_root.as_deref() == Some(p);

        let item = |id: ElementId, active: bool| {
            div()
                .id(id)
                .flex()
                .items_center()
                .gap(px(10.))
                .px_2()
                .py(px(6.))
                .rounded(px(8.))
                .when(active, |d| d.bg(c(SURFACE_HI)))
                .when(!active && !busy, |d| d.hover(|s| s.bg(c(HOVER))))
                .when(!busy, |d| d.cursor_pointer())
        };
        let text_col = |title: String, sub: String| {
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_w_0()
                .child(div().truncate().font_weight(FontWeight::MEDIUM).child(title))
                .child(div().truncate().text_size(px(11.5)).text_color(c(FAINT)).child(sub))
        };

        let vols: Vec<_> = self
            .volumes
            .iter()
            .enumerate()
            .map(|(i, v)| {
                let frac = if v.total > 0 { v.used() as f32 / v.total as f32 } else { 0. };
                item(ElementId::NamedInteger("vol".into(), i as u64), is_active(&v.mount))
                    .child(gauge_box(frac, usage_color(frac)))
                    .child(text_col(v.name.clone(), format!("{} free of {}", fmt::bytes(v.avail), fmt::bytes(v.total))))
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.scan_volume(i, cx)))
            })
            .collect();

        let home = self.home.clone();
        let home2 = self.home.clone();
        let home_active = is_active(&self.home);
        let open_dev = self.scanning.as_ref().map(|s| s.open_dev_tab).unwrap_or(self.tab == Tab::Dev);
        div()
            .flex()
            .flex_col()
            .flex_shrink_0()
            .w(px(SIDEBAR_W))
            .h_full()
            .bg(c(SIDEBAR))
            .border_r_1()
            .border_color(c(HAIRLINE))
            .pt(px(48.))
            .px(px(10.))
            .pb(px(12.))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_2()
                    .child(logo_mark())
                    .child(div().font_weight(FontWeight::SEMIBOLD).text_size(px(14.)).child("DevDisk")),
            )
            .child(section_label("Drives"))
            .children(vols)
            .child(section_label("Folders"))
            .child(
                item("scan-home".into(), home_active && !open_dev)
                    .child(glyph_box("~", c(ACCENT)))
                    .child(text_col("Home".into(), self.home.display().to_string()))
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.scan_folder(&home, false, cx))),
            )
            .child(
                item("scan-dev".into(), home_active && open_dev)
                    .child(glyph_box("</>", junk_color()))
                    .child(text_col("Dev junk".into(), "Caches & node_modules".into()))
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.scan_folder(&home2, true, cx))),
            )
            .child(
                item("choose".into(), false)
                    .child(glyph_box("+", c(MUTED)))
                    .child(text_col("Other folder…".into(), "Pick any folder to scan".into()))
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.choose_folder(cx))),
            )
            .child(div().flex_1())
            .child(
                div()
                    .id("fda")
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .px_2()
                    .py(px(8.))
                    .rounded(px(8.))
                    .cursor_pointer()
                    .hover(|s| s.bg(c(HOVER)))
                    .child(div().text_size(px(12.)).text_color(c(MUTED)).child("Full Disk Access ↗"))
                    .child(
                        div()
                            .text_size(px(11.))
                            .text_color(c(FAINT))
                            .child("Needed to measure Mail, Messages and some app data."),
                    )
                    .on_click(|_, _, cx: &mut App| {
                        cx.open_url("x-apple.systempreferences:com.apple.preference.security?Privacy_AllFiles")
                    }),
            )
    }

    fn render_main(&mut self, vw: f32, vh: f32, cx: &mut Context<Self>) -> impl IntoElement {
        let content: AnyElement = if let Some(s) = &self.scanning {
            self.render_progress(s, cx).into_any_element()
        } else if self.result.is_none() {
            self.render_welcome(cx).into_any_element()
        } else {
            match self.tab {
                Tab::Lens => self.render_lens(vw, vh, cx).into_any_element(),
                Tab::Dev => self.render_dev(cx).into_any_element(),
            }
        };
        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .h_full()
            .child(self.render_header(cx))
            .when_some(self.render_banner(cx), |d, b| d.child(b))
            .child(div().flex().flex_col().flex_1().min_h_0().child(content))
    }

    fn render_header(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let has_result = self.result.is_some() && self.scanning.is_none();
        let mut left = div().flex().flex_col().min_w_0().flex_1().gap(px(1.));
        if let (true, Some(res)) = (has_result, &self.result) {
            left = left
                .child(div().text_size(px(15.)).font_weight(FontWeight::SEMIBOLD).truncate().child(res.title.clone()))
                .child(div().text_size(px(11.5)).text_color(c(FAINT)).truncate().child(format!(
                    "{}  ·  {} files  ·  {} folders  ·  {}{}",
                    fmt::bytes(res.root.size),
                    fmt::count(res.root.items),
                    fmt::count(res.dirs),
                    fmt::duration(res.elapsed),
                    if res.denied > 0 { format!("  ·  {} unreadable", fmt::count(res.denied)) } else { String::new() }
                )));
        }
        let dev_total = self.dev_total();
        let tab = |id: &'static str, label: &'static str, active: bool| {
            div()
                .id(id)
                .flex()
                .items_center()
                .gap(px(6.))
                .px(px(12.))
                .h(px(26.))
                .rounded(px(6.))
                .cursor_pointer()
                .text_size(px(12.5))
                .whitespace_nowrap()
                .when(active, |d| d.bg(c(SURFACE_HI)).text_color(c(TEXT)).font_weight(FontWeight::MEDIUM).shadow_sm())
                .when(!active, |d| d.text_color(c(MUTED)).hover(|s| s.text_color(c(TEXT))))
                .child(label)
        };
        div()
            .flex()
            .flex_shrink_0()
            .items_center()
            .gap_3()
            .h(px(HEADER_H))
            .pt(px(6.))
            .px(px(PAD + 4.))
            .child(left)
            .when(has_result, |d| {
                d.child(
                    div()
                        .flex()
                        .gap(px(2.))
                        .p(px(3.))
                        .rounded(px(9.))
                        .bg(c(SURFACE))
                        .child(tab("tab-lens", "Space Lens", self.tab == Tab::Lens).on_click(cx.listener(
                            |this, _: &ClickEvent, _, cx| {
                                this.tab = Tab::Lens;
                                cx.notify()
                            },
                        )))
                        .child(
                            tab("tab-dev", "Cleanup", self.tab == Tab::Dev)
                                .when(dev_total > 0, |d| d.child(pill(fmt::bytes(dev_total), junk_color())))
                                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                    this.tab = Tab::Dev;
                                    cx.notify()
                                })),
                        ),
                )
                .child(
                    button("rescan", "↻  Rescan", Btn::Ghost, !self.trashing)
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.rescan(cx))),
                )
            })
    }

    fn render_banner(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let b = self.banner.as_ref()?;
        let color = match b.kind {
            BannerKind::Success => c(OK),
            BannerKind::Warn => c(WARN),
        };
        let log = trash::log_path(&self.home);
        Some(
            div()
                .flex()
                .flex_shrink_0()
                .items_start()
                .gap_3()
                .mx(px(PAD))
                .mb(px(8.))
                .px(px(14.))
                .py(px(10.))
                .rounded(px(10.))
                .bg(c(SURFACE))
                .border_1()
                .border_color(color.opacity(0.25))
                .child(dot(color, 8.).mt(px(5.)))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .flex_1()
                        .min_w_0()
                        .gap(px(3.))
                        .child(div().font_weight(FontWeight::MEDIUM).child(b.text.clone()))
                        .children(
                            b.details
                                .iter()
                                .map(|d| div().text_size(px(11.5)).text_color(c(MUTED)).truncate().child(d.clone())),
                        ),
                )
                .when(log.exists(), |d| {
                    d.child(
                        button("show-log", "Show log", Btn::Ghost, true)
                            .on_click(move |_, _, cx: &mut App| cx.reveal_path(&log)),
                    )
                })
                .child(icon_button("dismiss", "✕", true).on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                    this.banner = None;
                    cx.notify()
                })))
                .into_any_element(),
        )
    }

    fn render_welcome(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let busy = self.trashing;
        let drives = self.volumes.clone().into_iter().enumerate().map(|(i, v)| {
            let frac = if v.total > 0 { v.used() as f32 / v.total as f32 } else { 0. };
            div()
                .id(("welcome-vol", i))
                .flex()
                .flex_col()
                .gap(px(14.))
                .p(px(20.))
                .rounded(px(14.))
                .bg(c(SURFACE))
                .border_1()
                .border_color(c(HAIRLINE))
                .when(!busy, |d| d.cursor_pointer().hover(|s| s.border_color(c(ACCENT).opacity(0.6)).bg(c(HOVER))))
                .child(
                    div()
                        .flex()
                        .items_end()
                        .gap_3()
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .flex_1()
                                .min_w_0()
                                .gap(px(2.))
                                .child(div().text_size(px(16.)).font_weight(FontWeight::SEMIBOLD).truncate().child(v.name.clone()))
                                .child(caption(format!("{}  ·  {}", v.mount.display(), v.fs))),
                        )
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .items_end()
                                .gap(px(2.))
                                .child(
                                    div()
                                        .flex()
                                        .items_baseline()
                                        .gap_1()
                                        .child(div().text_size(px(16.)).font_weight(FontWeight::SEMIBOLD).child(fmt::bytes(v.used())))
                                        .child(caption("used")),
                                )
                                .child(caption(format!("{} free of {}", fmt::bytes(v.avail), fmt::bytes(v.total)))),
                        ),
                )
                .child(bar(frac, usage_color(frac), 6.))
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.scan_volume(i, cx)))
        });

        let home = self.home.clone();
        let home2 = self.home.clone();
        let quick = |id: &'static str, glyph: &'static str, color: Hsla, title: &'static str, sub: &'static str| {
            div()
                .id(id)
                .flex()
                .flex_col()
                .flex_1()
                .min_w_0()
                .gap(px(10.))
                .p(px(16.))
                .rounded(px(14.))
                .bg(c(SURFACE))
                .border_1()
                .border_color(c(HAIRLINE))
                .when(!busy, |d| d.cursor_pointer().hover(|s| s.border_color(color.opacity(0.6)).bg(c(HOVER))))
                .child(glyph_box(glyph, color))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(2.))
                        .child(div().font_weight(FontWeight::MEDIUM).child(title))
                        .child(caption(sub)),
                )
        };

        div()
            .id("welcome")
            .flex()
            .flex_col()
            .items_center()
            .size_full()
            .overflow_y_scroll()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .w_full()
                    .max_w(px(660.))
                    .px(px(24.))
                    .pt(px(36.))
                    .pb(px(40.))
                    .gap(px(14.))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(6.))
                            .mb(px(14.))
                            .child(div().text_size(px(26.)).font_weight(FontWeight::BOLD).child("What's using your disk?"))
                            .child(div().text_size(px(13.5)).text_color(c(MUTED)).child(
                                "Pick a drive to see every folder by size, or look for build caches and dev junk you can clear.",
                            )),
                    )
                    .children(drives)
                    .child(
                        div()
                            .flex()
                            .gap(px(14.))
                            .child(
                                quick("w-home", "~", c(ACCENT), "Home folder", "Everything in your user folder")
                                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.scan_folder(&home, false, cx))),
                            )
                            .child(
                                quick("w-dev", "</>", junk_color(), "Find dev junk", "node_modules, build caches, models")
                                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.scan_folder(&home2, true, cx))),
                            )
                            .child(
                                quick("w-other", "+", c(MUTED), "Other folder…", "Scan any folder you pick")
                                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.choose_folder(cx))),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .mt(px(14.))
                            .text_size(px(11.5))
                            .text_color(c(FAINT))
                            .child(dot(c(OK), 6.))
                            .child("Scanning is read-only. Cleanup only moves folders to the Trash after you confirm; nothing is deleted."),
                    ),
            )
    }

    fn render_progress(&self, s: &ActiveScan, cx: &mut Context<Self>) -> impl IntoElement {
        let p = &s.progress;
        let files = p.files.load(Ordering::Relaxed);
        let dirs = p.dirs.load(Ordering::Relaxed);
        let bytes = p.bytes.load(Ordering::Relaxed);
        let el = s.started.elapsed();
        let rate = files as f64 / el.as_secs_f64().max(0.001);
        // Indeterminate bar: a short segment sweeping left to right.
        let t = (el.as_millis() % 1400) as f32 / 1400.0;
        let seg = 0.28;
        let start = -seg + t * (1.0 + seg);
        let (left, width) = (start.max(0.0), (start + seg).min(1.0) - start.max(0.0));
        div()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(px(14.))
            .size_full()
            .pb(px(HEADER_H))
            .child(caption(format!("Scanning {}", s.title)))
            .child(div().text_size(px(40.)).font_weight(FontWeight::BOLD).child(fmt::bytes(bytes)))
            .child(
                div()
                    .relative()
                    .w(px(300.))
                    .h(px(3.))
                    .rounded_full()
                    .bg(hsla(0., 0., 1., 0.07))
                    .overflow_hidden()
                    .child(
                        div()
                            .absolute()
                            .top_0()
                            .h_full()
                            .rounded_full()
                            .bg(c(ACCENT))
                            .left(relative(left))
                            .w(relative(width.max(0.0))),
                    ),
            )
            .child(div().text_color(c(MUTED)).child(format!(
                "{} files  ·  {} folders  ·  {}/s  ·  {}",
                fmt::count(files),
                fmt::count(dirs),
                fmt::count(rate as u64),
                fmt::duration(el)
            )))
            .child(div().text_size(px(11.5)).text_color(c(FAINT)).child(s.root.display().to_string()))
            .child(div().mt_2().child(button("cancel-scan", "Cancel", Btn::Secondary, true).on_click(cx.listener(
                |this, _: &ClickEvent, _, cx| {
                    this.cancel_scan();
                    cx.notify()
                },
            ))))
    }

    // ---------------------------------------------------------------- lens

    fn render_lens(&mut self, vw: f32, vh: f32, cx: &mut Context<Self>) -> impl IntoElement {
        let banner_h = match &self.banner {
            Some(b) => 50.0 + b.details.len() as f32 * 17.0,
            None => 0.0,
        };
        let tm_w = (vw - SIDEBAR_W - LIST_W - PAD * 3.0).max(100.0);
        let tm_h = (vh - HEADER_H - CRUMB_H - STATUS_H - PAD - banner_h).max(100.0);

        let count = self.visible.len();
        let list = uniform_list(
            "lens-list",
            count,
            cx.processor(|this, range: std::ops::Range<usize>, _window, cx| {
                range.map(|ix| this.lens_row(ix, cx)).collect::<Vec<_>>()
            }),
        )
        .track_scroll(self.list_scroll.clone())
        .flex_1();

        let cur = self.current();
        let empty_note = match cur {
            Some(n) if n.flags & F_DENIED != 0 && n.children.is_empty() => {
                Some("No permission to read this folder. Grant DevDisk Full Disk Access and rescan.")
            }
            Some(n) if n.children.is_empty() => Some("This folder is empty."),
            _ => None,
        };

        div()
            .flex()
            .flex_col()
            .size_full()
            .child(self.render_crumbs(cx))
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .gap(px(PAD))
                    .px(px(PAD))
                    .pb(px(PAD))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_shrink_0()
                            .w(px(LIST_W))
                            .h_full()
                            .rounded(px(12.))
                            .bg(c(SURFACE))
                            .overflow_hidden()
                            .child(
                                div()
                                    .flex()
                                    .flex_shrink_0()
                                    .items_center()
                                    .h(px(30.))
                                    .px(px(14.))
                                    .gap_2()
                                    .text_size(px(11.))
                                    .text_color(c(FAINT))
                                    .border_b_1()
                                    .border_color(c(HAIRLINE))
                                    .child(div().flex_1().child("Name"))
                                    .child(div().w(px(70.)).flex().justify_end().child("Size"))
                                    .child(div().w(px(42.)).flex().justify_end().child("%"))
                                    .child(div().w(px(24.))),
                            )
                            .when_some(empty_note, |d, t| d.child(div().p_4().text_color(c(MUTED)).child(t)))
                            .child(list),
                    )
                    .child(self.render_treemap(tm_w, tm_h, cx)),
            )
            .child(self.render_status())
    }

    fn render_crumbs(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(res) = &self.result else { return div() };
        let mut names: Vec<String> = vec![res.title.clone()];
        let mut cur = &res.root;
        for &i in &self.nav {
            cur = &cur.children[i as usize];
            names.push(cur.name.to_string());
        }
        let depth = self.nav.len();
        let path = self.current_path();
        let (size, items) = self.current().map(|n| (n.size, n.items)).unwrap_or((0, 0));
        let mut crumbs = div().flex().items_center().gap(px(2.)).min_w_0().flex_1().overflow_hidden();
        let n = names.len();
        let sep = || div().flex_shrink_0().text_color(c(FAINT)).px(px(2.)).child("/");
        // Collapse very deep paths: first, …, last 4.
        for (k, name) in names.into_iter().enumerate() {
            if n > 6 && k > 0 && k < n - 4 {
                if k == 1 {
                    crumbs = crumbs.child(sep()).child(div().text_color(c(FAINT)).child("…"));
                }
                continue;
            }
            if k > 0 {
                crumbs = crumbs.child(sep());
            }
            let last = k == n - 1;
            crumbs = crumbs.child(
                div()
                    .id(("crumb", k))
                    .px(px(6.))
                    .py(px(3.))
                    .rounded(px(6.))
                    .truncate()
                    .when(last, |d| d.font_weight(FontWeight::MEDIUM))
                    .when(!last, |d| {
                        d.text_color(c(MUTED)).cursor_pointer().hover(|s| s.text_color(c(TEXT)).bg(c(SURFACE)))
                    })
                    .child(name)
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.go_to_depth(k, cx))),
            );
        }
        div()
            .flex()
            .flex_shrink_0()
            .items_center()
            .gap(px(6.))
            .h(px(CRUMB_H))
            .px(px(PAD - 4.))
            .child(icon_button("up", "←", depth > 0).on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                if depth > 0 {
                    this.go_to_depth(depth - 1, cx)
                }
            })))
            .child(crumbs)
            .child(
                div()
                    .flex_shrink_0()
                    .text_size(px(11.5))
                    .text_color(c(FAINT))
                    .child(format!("{}  ·  {} files", fmt::bytes(size), fmt::count(items))),
            )
            .child(
                button("reveal-cur", "Show in Finder", Btn::Ghost, true)
                    .on_click(move |_, _, cx: &mut App| cx.reveal_path(&path)),
            )
    }

    fn lens_row(&mut self, ix: usize, cx: &mut Context<Self>) -> AnyElement {
        let Some(&child_ix) = self.visible.get(ix) else { return div().into_any_element() };
        let base = self.current_path();
        let Some(cur) = self.current() else { return div().into_any_element() };
        let Some(child) = cur.children.get(child_ix as usize) else { return div().into_any_element() };
        let parent_size = cur.size.max(1);
        let frac = child.size as f32 / parent_size as f32;
        let largest = self.visible.first().map(|&i| cur.children[i as usize].size).unwrap_or(1).max(1);
        let rel = child.size as f32 / largest as f32;
        let color = node_color(child, child_ix as usize);
        let openable = child.kind == Kind::Dir && !child.trashed() && !child.children.is_empty();
        let name = child.name.to_string();
        let size_s = fmt::bytes(child.size);
        let rule_badge = (child.rule != NO_RULE).then(|| {
            let r = rules::rule(child.rule);
            (r.category.label(), risk_color(r.risk))
        });
        let note = if child.flags & F_OTHER_VOLUME != 0 {
            Some("other volume")
        } else if child.flags & F_SKIPPED != 0 {
            Some("not scanned")
        } else if child.flags & F_DENIED != 0 && child.children.is_empty() {
            Some("no access")
        } else {
            None
        };
        let kind = child.kind;
        let items = child.items;
        let reveal = if kind == Kind::Others { base.clone() } else { base.join(&*child.name) };
        let hover_text: SharedString = match kind {
            Kind::Dir => format!("{}  ·  {}  ·  {} files", self.display_path(&reveal), size_s, fmt::count(items)),
            Kind::Others => format!("{} in {}  ·  {}", name, self.display_path(&base), size_s),
            Kind::File => format!("{}  ·  {}", self.display_path(&reveal), size_s),
        }
        .into();

        div()
            .id(("lens-row", ix))
            .group("lens-row")
            .relative()
            .h(px(ROW_H))
            .w_full()
            .flex()
            .items_center()
            .hover(|s| s.bg(c(HOVER)))
            .on_hover(cx.listener(move |this, h: &bool, _, cx| {
                if *h {
                    this.hover = Some(hover_text.clone());
                    cx.notify();
                }
            }))
            .child(
                div()
                    .id(("lens-open", ix))
                    .flex()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .items_center()
                    .gap(px(9.))
                    .pl(px(14.))
                    .when(openable, |d| d.cursor_pointer())
                    .child(div().flex_shrink_0().size(px(8.)).rounded(px(2.5)).bg(color))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .when(kind == Kind::Others, |d| d.text_color(c(MUTED)))
                            .child(name),
                    )
                    .when_some(rule_badge, |d, (label, col)| d.child(pill(label, col)))
                    .when_some(note, |d, n| d.child(pill(n, c(MUTED))))
                    .child(
                        div()
                            .w(px(12.))
                            .flex_shrink_0()
                            .text_color(c(FAINT))
                            .when(openable, |d| d.child("›")),
                    )
                    .child(div().w(px(48.)).flex_shrink_0().child(bar(rel.max(0.03), color.opacity(0.85), 4.)))
                    .child(div().w(px(70.)).flex_shrink_0().flex().justify_end().child(size_s))
                    .child(
                        div()
                            .w(px(42.))
                            .flex_shrink_0()
                            .flex()
                            .justify_end()
                            .text_size(px(11.5))
                            .text_color(c(FAINT))
                            .child(format!("{:.0}%", (frac * 100.).round())),
                    )
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.open_child(child_ix, cx))),
            )
            .child(
                div()
                    .id(("lens-reveal", ix))
                    .flex()
                    .flex_shrink_0()
                    .items_center()
                    .justify_center()
                    .w(px(24.))
                    .h_full()
                    .mr(px(6.))
                    .text_color(c(FAINT))
                    .opacity(0.)
                    .group_hover("lens-row", |s| s.opacity(1.))
                    .cursor_pointer()
                    .hover(|s| s.text_color(c(ACCENT)))
                    .child("↗")
                    .on_click(move |_, _, cx: &mut App| cx.reveal_path(&reveal)),
            )
            .into_any_element()
    }

    fn render_treemap(&mut self, w: f32, h: f32, cx: &mut Context<Self>) -> impl IntoElement {
        let mut container = div().id("treemap").relative().flex_1().min_w_0().h_full();
        let Some(cur) = self.current() else { return container };
        let base = self.current_path();

        const MAX_TILES: usize = 60;
        let mut idxs: Vec<u32> = Vec::new();
        let mut vals: Vec<f64> = Vec::new();
        let mut rest = 0u64;
        for &ci in &self.visible {
            let ch = &cur.children[ci as usize];
            if ch.size == 0 {
                continue;
            }
            if idxs.len() < MAX_TILES && (ch.size as f64) > cur.size as f64 * 0.0004 {
                idxs.push(ci);
                vals.push(ch.size as f64);
            } else {
                rest += ch.size;
            }
        }
        let has_rest = rest > 0;
        if has_rest {
            vals.push(rest as f64);
        }
        if vals.is_empty() {
            return container.child(
                div()
                    .size_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(12.))
                    .bg(c(SURFACE))
                    .text_color(c(FAINT))
                    .child("Nothing to show"),
            );
        }
        let rects = treemap::squarify(&vals, Rect { x: 0., y: 0., w, h });

        let mut tiles: Vec<AnyElement> = Vec::with_capacity(rects.len());
        for (k, r) in rects.iter().enumerate() {
            let is_rest = has_rest && k == rects.len() - 1;
            let (name, size, color, node_ix) = if is_rest {
                (
                    format!("{} smaller items", self.visible.len().saturating_sub(idxs.len())),
                    rest,
                    hsla(0.65, 0.04, 0.24, 1.),
                    None,
                )
            } else {
                let ch = &cur.children[idxs[k] as usize];
                (ch.name.to_string(), ch.size, node_color(ch, idxs[k] as usize), Some(idxs[k]))
            };
            let show_label = r.w > 84. && r.h > 28.;
            let show_size = r.w > 150.;
            let size_s = fmt::bytes(size);
            let full_path = node_ix.map(|_| base.join(&name)).unwrap_or_else(|| base.clone());
            let hover_text: SharedString = format!("{}  ·  {}", self.display_path(&full_path), size_s).into();

            // Second level: children of big folders.
            let mut nested: Vec<AnyElement> = Vec::new();
            let mut nested_area = false;
            if let Some(ni) = node_ix {
                let ch = &cur.children[ni as usize];
                let inner_w = r.w - 10.;
                let inner_h = r.h - 30.;
                if ch.kind == Kind::Dir && !ch.children.is_empty() && inner_w > 70. && inner_h > 46. && ch.rule == NO_RULE {
                    nested_area = true;
                    let mut nidx: Vec<usize> = Vec::new();
                    let mut nvals: Vec<f64> = Vec::new();
                    let mut nrest = 0u64;
                    for (j, g) in ch.children.iter().enumerate() {
                        if g.size == 0 || g.trashed() {
                            continue;
                        }
                        if nidx.len() < 24 && g.size as f64 > ch.size as f64 * 0.01 {
                            nidx.push(j);
                            nvals.push(g.size as f64);
                        } else {
                            nrest += g.size;
                        }
                    }
                    if nrest > 0 {
                        nvals.push(nrest as f64);
                    }
                    let nrects = treemap::squarify(&nvals, Rect { x: 0., y: 0., w: inner_w, h: inner_h });
                    for (j, nr) in nrects.iter().enumerate() {
                        let is_nrest = nrest > 0 && j == nrects.len() - 1;
                        let (gname, gsize, gcolor, gix) = if is_nrest {
                            ("other items".to_string(), nrest, hsla(color.h, color.s * 0.4, color.l - 0.04, 1.), None)
                        } else {
                            let g = &ch.children[nidx[j]];
                            (g.name.to_string(), g.size, nested_color(g, color, j), Some(nidx[j] as u32))
                        };
                        let glabel = nr.w > 80. && nr.h > 22.;
                        let gpath = base.join(&name).join(&gname);
                        let ghover: SharedString = format!(
                            "{}  ·  {}",
                            if is_nrest { self.display_path(&base.join(&name)) } else { self.display_path(&gpath) },
                            fmt::bytes(gsize)
                        )
                        .into();
                        let gopenable = gix
                            .map(|gi| {
                                let g = &ch.children[gi as usize];
                                g.kind == Kind::Dir && !g.children.is_empty()
                            })
                            .unwrap_or(false);
                        nested.push(
                            div()
                                .id(ElementId::NamedInteger("tm-n".into(), (k * 1000 + j) as u64))
                                .absolute()
                                .left(relative(nr.x / inner_w))
                                .top(relative(nr.y / inner_h))
                                .w(relative(nr.w / inner_w))
                                .h(relative(nr.h / inner_h))
                                .p(px(1.5))
                                .child(
                                    div()
                                        .size_full()
                                        .rounded(px(4.))
                                        .bg(gcolor)
                                        .border_1()
                                        .border_color(clear())
                                        .hover(|s| s.border_color(hsla(0., 0., 1., 0.55)))
                                        .overflow_hidden()
                                        .px(px(6.))
                                        .pt(px(3.))
                                        .when(glabel, |d| {
                                            d.child(
                                                div()
                                                    .text_size(px(11.))
                                                    .text_color(hsla(0., 0., 1., 0.78))
                                                    .truncate()
                                                    .child(gname.clone()),
                                            )
                                        }),
                                )
                                .on_hover(cx.listener(move |this, hv: &bool, _, cx| {
                                    if *hv {
                                        this.hover = Some(ghover.clone());
                                        cx.notify();
                                    }
                                }))
                                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                    cx.stop_propagation();
                                    this.open_child(ni, cx);
                                    if gopenable {
                                        if let Some(gi) = gix {
                                            this.open_child(gi, cx);
                                        }
                                    }
                                }))
                                .into_any_element(),
                        );
                    }
                }
            }

            let tile = div()
                .id(("tm", k))
                .absolute()
                .left(relative(r.x / w))
                .top(relative(r.y / h))
                .w(relative(r.w / w))
                .h(relative(r.h / h))
                .p(px(2.))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .size_full()
                        .rounded(px(7.))
                        .bg(color)
                        .border_1()
                        .border_color(clear())
                        .hover(|s| s.border_color(hsla(0., 0., 1., 0.7)))
                        .overflow_hidden()
                        .when(show_label, |d| {
                            d.child(
                                div()
                                    .flex()
                                    .flex_shrink_0()
                                    .items_center()
                                    .gap_2()
                                    .px(px(8.))
                                    .h(px(26.))
                                    .text_size(px(12.))
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .truncate()
                                            .font_weight(FontWeight::MEDIUM)
                                            .text_color(hsla(0., 0., 1., 0.95))
                                            .child(name.clone()),
                                    )
                                    .when(show_size, |d| {
                                        d.child(
                                            div()
                                                .flex_shrink_0()
                                                .text_size(px(11.))
                                                .text_color(hsla(0., 0., 1., 0.62))
                                                .child(size_s.clone()),
                                        )
                                    }),
                            )
                        })
                        .when(nested_area, |d| {
                            d.child(div().relative().flex_1().mx(px(3.)).mb(px(3.)).children(nested))
                        }),
                )
                .on_hover(cx.listener(move |this, hv: &bool, _, cx| {
                    if *hv {
                        this.hover = Some(hover_text.clone());
                        cx.notify();
                    }
                }))
                .when_some(node_ix, |d, ni| {
                    d.cursor_pointer().on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.open_child(ni, cx)))
                });
            tiles.push(tile.into_any_element());
        }
        container = container.children(tiles);
        container
    }

    fn render_status(&self) -> impl IntoElement {
        let mut note: Option<String> = None;
        if let Some(res) = &self.result {
            if let Some(used) = res.volume_used {
                if used > self.scanned_total {
                    note = Some(format!(
                        "≈ {} of used space isn't visible to the scan (system data, snapshots, purgeable space, folders without access)",
                        fmt::bytes(used - self.scanned_total)
                    ));
                }
            }
        }
        let text = self
            .hover
            .clone()
            .map(|s| s.to_string())
            .or(note)
            .unwrap_or_else(|| "Click a folder to open it  ·  ↗ shows it in Finder".into());
        let legend = [
            ("Folders", folder_color()),
            ("Dev junk", junk_color()),
            ("Video", file_color(HUE_VIDEO)),
            ("Images", file_color(HUE_IMAGE)),
            ("Audio", file_color(HUE_AUDIO)),
            ("Archives", file_color(HUE_ARCHIVE)),
            ("Models", file_color(HUE_MODEL)),
            ("Data", file_color(HUE_DATA)),
        ];
        div()
            .flex()
            .flex_shrink_0()
            .items_center()
            .gap(px(12.))
            .h(px(STATUS_H))
            .px(px(PAD + 2.))
            .border_t_1()
            .border_color(c(HAIRLINE))
            .text_size(px(11.))
            .text_color(c(FAINT))
            .child(div().flex_1().min_w_0().truncate().child(text))
            .children(legend.into_iter().map(|(l, col)| {
                div().flex().flex_shrink_0().items_center().gap(px(5.)).child(dot(col, 7.)).child(l)
            }))
    }

    // ----------------------------------------------------------------- dev

    fn render_dev(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let n_sel = self.selected.len();
        let sel_total = self.selected_total();
        let busy = self.trashing || self.scanning.is_some();
        let root_note = self.result.as_ref().map(|r| self.display_path(&r.root_path)).unwrap_or_default();

        if self.groups.is_empty() {
            let home = self.home.clone();
            return div()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap_3()
                .size_full()
                .pb(px(HEADER_H))
                .child(glyph_box("✓", c(OK)))
                .child(div().text_size(px(16.)).font_weight(FontWeight::SEMIBOLD).child("No dev junk here"))
                .child(caption(format!("Nothing matched the cleanup rules in {root_note}")))
                .child(
                    button("dev-scan-home", "Scan Home for dev junk", Btn::Primary, !busy)
                        .mt_2()
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.scan_folder(&home, true, cx))),
                )
                .into_any_element();
        }

        let rows = self.dev_rows.len();
        let list = uniform_list(
            "dev-list",
            rows,
            cx.processor(|this, range: std::ops::Range<usize>, _window, cx| {
                range.map(|ix| this.dev_row(ix, cx)).collect::<Vec<_>>()
            }),
        )
        .track_scroll(self.dev_scroll.clone())
        .flex_1();

        let legend = [
            (Risk::Safe, "rebuilt automatically"),
            (Risk::Caution, "needs a re-download"),
            (Risk::InfoOnly, "shown only, never removed"),
        ];

        div()
            .flex()
            .flex_col()
            .size_full()
            .px(px(PAD))
            .pb(px(PAD))
            .gap(px(12.))
            .child(
                div()
                    .flex()
                    .flex_shrink_0()
                    .items_end()
                    .gap_2()
                    .pt(px(4.))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w_0()
                            .gap(px(4.))
                            .child(
                                div()
                                    .flex()
                                    .items_baseline()
                                    .gap_2()
                                    .child(
                                        div()
                                            .text_size(px(26.))
                                            .font_weight(FontWeight::BOLD)
                                            .child(fmt::bytes(self.dev_total())),
                                    )
                                    .child(div().text_color(c(MUTED)).truncate().child(format!(
                                        "in {} folders under {root_note}",
                                        fmt::count(self.groups.iter().map(|g| g.items.len() as u64).sum())
                                    ))),
                            )
                            .child(div().flex().gap(px(14.)).children(legend.into_iter().map(|(r, t)| {
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(5.))
                                    .text_size(px(11.5))
                                    .child(dot(risk_color(r), 6.))
                                    .child(div().text_color(c(MUTED)).child(r.label()))
                                    .child(div().text_color(c(FAINT)).child(t))
                            }))),
                    )
                    .child(
                        button("sel-safe", "Select all Safe", Btn::Secondary, !busy)
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.select_where(|_| true, cx))),
                    )
                    .child(
                        button("sel-stale", "Safe, unused 30+ days", Btn::Secondary, !busy).on_click(cx.listener(
                            |this, _: &ClickEvent, _, cx| this.select_where(|d| fmt::age_days(d.modified) >= 30, cx),
                        )),
                    )
                    .child(button("sel-none", "Clear", Btn::Ghost, n_sel > 0).on_click(cx.listener(
                        |this, _: &ClickEvent, _, cx| {
                            this.selected.clear();
                            cx.notify()
                        },
                    ))),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h_0()
                    .rounded(px(12.))
                    .bg(c(SURFACE))
                    .overflow_hidden()
                    .child(list),
            )
            .child(
                div()
                    .flex()
                    .flex_shrink_0()
                    .items_center()
                    .gap_3()
                    .px(px(16.))
                    .py(px(12.))
                    .rounded(px(12.))
                    .bg(c(SURFACE))
                    .border_1()
                    .border_color(if n_sel > 0 { c(DANGER).opacity(0.35) } else { c(HAIRLINE) })
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w_0()
                            .gap(px(2.))
                            .child(div().font_weight(FontWeight::MEDIUM).child(if n_sel == 0 {
                                "Nothing selected".to_string()
                            } else {
                                format!(
                                    "{} folder{} selected  ·  {}",
                                    fmt::count(n_sel as u64),
                                    if n_sel == 1 { "" } else { "s" },
                                    fmt::bytes(sel_total)
                                )
                            }))
                            .child(caption("Selected folders go to the Trash, so you can still restore them.")),
                    )
                    .child(
                        button("review", "Move to Trash…", Btn::Danger, n_sel > 0 && !busy)
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.open_confirm(cx))),
                    ),
            )
            .into_any_element()
    }

    fn dev_row(&mut self, ix: usize, cx: &mut Context<Self>) -> AnyElement {
        let Some(row) = self.dev_rows.get(ix).copied() else { return div().into_any_element() };
        match row {
            DevRow::Header(gi) => {
                let g = &self.groups[gi];
                let collapsed = self.collapsed.contains(&g.cat);
                let check = self.group_check(gi);
                let any_selectable = g.items.iter().any(|&i| self.selectable(i));
                let label = g.cat.label();
                let hue = g.cat.hue();
                let info = format!("{} {}", fmt::count(g.items.len() as u64), if g.items.len() == 1 { "folder" } else { "folders" });
                let total = fmt::bytes(g.total);
                div()
                    .id(("dev-h", ix))
                    .h(px(DEV_ROW_H))
                    .w_full()
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .px(px(14.))
                    .when(ix > 0, |d| d.border_t_1().border_color(c(HAIRLINE)))
                    .child(
                        div()
                            .id(("dev-h-check", ix))
                            .p_1()
                            .when(any_selectable, |d| d.cursor_pointer())
                            .child(checkbox(check, any_selectable))
                            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                if any_selectable {
                                    this.toggle_group(gi, cx)
                                }
                            })),
                    )
                    .child(
                        div()
                            .id(("dev-h-toggle", ix))
                            .flex()
                            .flex_1()
                            .min_w_0()
                            .h_full()
                            .items_center()
                            .gap(px(10.))
                            .cursor_pointer()
                            .child(dot(hsla(hue, 0.6, 0.58, 1.), 8.))
                            .child(div().font_weight(FontWeight::SEMIBOLD).child(label))
                            .child(div().text_size(px(11.5)).text_color(c(FAINT)).child(info))
                            .child(div().flex_1())
                            .child(div().font_weight(FontWeight::SEMIBOLD).child(total))
                            .child(
                                div()
                                    .w(px(24.))
                                    .flex()
                                    .justify_center()
                                    .text_color(c(FAINT))
                                    .child(if collapsed { "›" } else { "⌄" }),
                            )
                            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.toggle_collapsed(gi, cx))),
                    )
                    .into_any_element()
            }
            DevRow::Item(i) => {
                let Some(res) = &self.result else { return div().into_any_element() };
                let d = &res.dev_items[i];
                let r = rules::rule(d.rule);
                let selectable = self.selectable(i);
                let selected = self.selected.contains(&i);
                let folder = d.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                let parent = d.path.parent().map(|p| format!("{}/", self.display_path(p))).unwrap_or_default();
                let path = d.path.clone();
                let sub = format!(
                    "{}  ·  {} files  ·  modified {}  ·  {}",
                    r.label,
                    fmt::count(d.items),
                    fmt::age(d.modified),
                    r.note
                );
                let size_s = fmt::bytes(d.size);
                let risk = r.risk;
                div()
                    .id(("dev-i", ix))
                    .group("dev-row")
                    .h(px(DEV_ROW_H))
                    .w_full()
                    .flex()
                    .items_center()
                    .when(selected, |d| d.bg(c(ACCENT).opacity(0.07)))
                    .hover(|s| s.bg(c(HOVER)))
                    .child(
                        div()
                            .id(("dev-i-main", ix))
                            .flex()
                            .flex_1()
                            .min_w_0()
                            .h_full()
                            .items_center()
                            .gap(px(12.))
                            .pl(px(18.))
                            .pr_2()
                            .when(selectable, |d| d.cursor_pointer())
                            .child(checkbox(if selected { Check::On } else { Check::Off }, selectable))
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .flex_1()
                                    .min_w_0()
                                    .gap(px(2.))
                                    .child(
                                        div()
                                            .flex()
                                            .min_w_0()
                                            .child(div().flex_shrink().min_w_0().truncate().text_color(c(MUTED)).child(parent))
                                            .child(div().flex_shrink_0().font_weight(FontWeight::MEDIUM).child(folder)),
                                    )
                                    .child(div().text_size(px(11.5)).text_color(c(FAINT)).truncate().child(sub)),
                            )
                            .child(
                                div()
                                    .flex()
                                    .flex_shrink_0()
                                    .items_center()
                                    .gap(px(5.))
                                    .w(px(70.))
                                    .text_size(px(11.5))
                                    .text_color(risk_color(risk))
                                    .child(dot(risk_color(risk), 6.))
                                    .child(risk.label()),
                            )
                            .child(
                                div()
                                    .w(px(76.))
                                    .flex_shrink_0()
                                    .flex()
                                    .justify_end()
                                    .font_weight(FontWeight::MEDIUM)
                                    .child(size_s),
                            )
                            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.toggle_item(i, cx))),
                    )
                    .child(
                        div()
                            .id(("dev-i-reveal", ix))
                            .flex()
                            .flex_shrink_0()
                            .items_center()
                            .justify_center()
                            .w(px(28.))
                            .h_full()
                            .mr(px(8.))
                            .text_color(c(FAINT))
                            .opacity(0.)
                            .group_hover("dev-row", |s| s.opacity(1.))
                            .cursor_pointer()
                            .hover(|s| s.text_color(c(ACCENT)))
                            .child("↗")
                            .on_click(move |_, _, cx: &mut App| cx.reveal_path(&path)),
                    )
                    .into_any_element()
            }
        }
    }

    // ------------------------------------------------------------- confirm

    fn render_confirm(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let Some(conf) = &self.confirm else { return div().into_any_element() };
        let Some(res) = &self.result else { return div().into_any_element() };
        let armed = conf.opened.elapsed() >= CONFIRM_ARM_DELAY && !self.trashing;
        let n = conf.items.len();
        let rows = conf.items.iter().map(|&i| {
            let d = &res.dev_items[i];
            let r = rules::rule(d.rule);
            div()
                .flex()
                .items_center()
                .gap(px(10.))
                .px(px(12.))
                .py(px(7.))
                .border_b_1()
                .border_color(c(HAIRLINE))
                .child(dot(risk_color(r.risk), 6.))
                .child(div().flex_1().min_w_0().truncate().text_size(px(12.)).child(self.display_path(&d.path)))
                .child(div().flex_shrink_0().text_size(px(12.)).text_color(c(MUTED)).child(fmt::bytes(d.size)))
        });
        let confirm_label = if self.trashing {
            "Moving to Trash…".to_string()
        } else {
            format!("Move {} to Trash", if n == 1 { "1 folder".to_string() } else { format!("{n} folders") })
        };

        div()
            .id("confirm-overlay")
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .bg(hsla(0., 0., 0., 0.55))
            .occlude()
            .child(
                div()
                    .id("confirm-panel")
                    .flex()
                    .flex_col()
                    .gap(px(14.))
                    .w(px(580.))
                    .max_h(relative(0.86))
                    .p(px(22.))
                    .rounded(px(16.))
                    .bg(c(SURFACE))
                    .border_1()
                    .border_color(c(BORDER))
                    .shadow_2xl()
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(4.))
                            .child(div().text_size(px(17.)).font_weight(FontWeight::SEMIBOLD).child(format!(
                                "Move {} to the Trash?",
                                if n == 1 { "1 folder".to_string() } else { format!("{n} folders") }
                            )))
                            .child(div().text_color(c(MUTED)).child(format!("{} total", fmt::bytes(conf.total)))),
                    )
                    .child(div().text_size(px(12.5)).text_color(c(MUTED)).child(
                        "Nothing is permanently deleted. You can restore these from the Trash, and the space is only freed \
                         when you empty it yourself. Each folder is checked again right before it moves; anything that no \
                         longer matches its rule is skipped.",
                    ))
                    .when(conf.caution > 0, |d| {
                        d.child(
                            div()
                                .flex()
                                .items_center()
                                .gap_2()
                                .px(px(12.))
                                .py(px(9.))
                                .rounded(px(9.))
                                .bg(c(WARN).opacity(0.09))
                                .text_size(px(12.5))
                                .text_color(c(WARN))
                                .child(dot(c(WARN), 6.))
                                .child(format!(
                                    "{} marked Review need a re-download or re-install to get back (models, venvs, toolchains…).",
                                    if conf.caution == 1 { "1 item".to_string() } else { format!("{} items", conf.caution) }
                                )),
                        )
                    })
                    .child(
                        div()
                            .id("confirm-list")
                            .flex()
                            .flex_col()
                            .max_h(px(300.))
                            .overflow_y_scroll()
                            .rounded(px(10.))
                            .bg(c(BG))
                            .border_1()
                            .border_color(c(HAIRLINE))
                            .children(rows),
                    )
                    .child(
                        div()
                            .flex()
                            .justify_end()
                            .gap_2()
                            .pt(px(4.))
                            .child(button("confirm-cancel", "Cancel", Btn::Secondary, !self.trashing).on_click(
                                cx.listener(|this, _: &ClickEvent, _, cx| {
                                    if !this.trashing {
                                        this.confirm = None;
                                        cx.notify();
                                    }
                                }),
                            ))
                            .child(button("confirm-go", confirm_label, Btn::Danger, armed).on_click(cx.listener(
                                |this, ev: &ClickEvent, _, cx| {
                                    // Only a real mouse click; never a keyboard-triggered click.
                                    if matches!(ev, ClickEvent::Mouse(_)) {
                                        this.run_trash(cx);
                                    }
                                },
                            ))),
                    ),
            )
            .into_any_element()
    }
}
