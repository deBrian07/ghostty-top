use std::collections::{BTreeMap, HashMap, VecDeque};
use std::env;
use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const GREEN: &str = "\x1b[38;5;114m";
const CYAN: &str = "\x1b[38;5;81m";
const YELLOW: &str = "\x1b[38;5;221m";
const DIM: &str = "\x1b[2m";
const BOLD: &str = "\x1b[1m";
const RESET: &str = "\x1b[0m";
const SELECTED: &str = "\x1b[48;5;236m";
const UNDERLINE: &str = "\x1b[4m";
const RED: &str = "\x1b[38;5;203m";
const WARNING: &str = "\x1b[48;5;52m\x1b[38;5;231m";
const ACTIVE_HEADER: &str = "\x1b[1;38;5;232;48;5;114m";
const HISTORY_WINDOW: Duration = Duration::from_secs(15 * 60);
const MIN_LEAK_DURATION: Duration = Duration::from_secs(3 * 60);
const MIN_LEAK_SAMPLES: usize = 8;
const MIN_LEAK_GROWTH_KIB: i64 = 48 * 1024;
const MIN_LEAK_GROWTH_RATIO: f64 = 0.10;
const MIN_LEAK_SLOPE_KIB_PER_MIN: f64 = 4.0 * 1024.0;
const MIN_LEAK_R_SQUARED: f64 = 0.70;
const MIN_UPWARD_SAMPLE_RATIO: f64 = 0.70;
const LEAK_CONFIRMATION_WINDOWS: u8 = 3;
const LEAK_CLEAR_WINDOWS: u8 = 10;
const CALENDAR_GUTTER: usize = 4;
const CALENDAR_CELL_WIDTH: usize = 2;
const CALENDAR_MIN_WEEKS: usize = 4;
const CALENDAR_MAX_WEEKS: usize = 53;
const CALENDAR_BLOCK: &str = "■";
const MONTH_NAMES: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];
// Highlights recolour the square rather than filling its cell: a background
// covers the gap beside the glyph too, which reads as a box sitting off-centre.
const CALENDAR_HOVER: &str = "\x1b[1;38;5;159m";
const CALENDAR_SELECTED: &str = "\x1b[1;38;5;231m";
const CALENDAR_RAMP: [&str; 5] = [
    "\x1b[38;5;237m",
    "\x1b[38;5;25m",
    "\x1b[38;5;32m",
    "\x1b[38;5;39m",
    "\x1b[38;5;81m",
];
/// How long the user may sit untouched before their focused time stops counting.
#[cfg(target_os = "macos")]
const MAX_IDLE: Duration = Duration::from_secs(5 * 60);
/// A tab cannot be focused for longer than the day it is recorded against, so
/// any larger value is corrupt. Bounding it here keeps every later total well
/// inside u64 instead of overflowing on a hand-edited or truncated file.
const MAX_DAILY_SECONDS: u64 = 24 * 60 * 60;
const TAB_LABEL_REFRESH: Duration = Duration::from_secs(15);
#[cfg(target_os = "macos")]
const INVENTORY_TIMEOUT: Duration = Duration::from_secs(8);
const USAGE_POLL_INTERVAL: Duration = Duration::from_secs(5);
const USAGE_FLUSH_INTERVAL: Duration = Duration::from_secs(30);
#[cfg(target_os = "macos")]
const USAGE_SAMPLE_TIMEOUT: Duration = Duration::from_secs(1);
#[cfg(target_os = "macos")]
const LAUNCH_AGENT_LABEL: &str = "com.debrian07.ghostty-top.tracker";
const INVENTORY_POLL_INTERVAL: Duration = Duration::from_secs(60);
const INVENTORY_HEARTBEAT: Duration = Duration::from_secs(15 * 60);
const RESOURCE_HISTORY_INTERVAL: Duration = Duration::from_secs(5 * 60);
const HISTORY_SCHEMA_VERSION: &str = "1";

// Platform differences live here so the rest of the file stays one
// implementation. macOS behaviour is unchanged: every macOS branch is the code
// that was already here.

/// `stty` names the device differently on each platform.
#[cfg(target_os = "macos")]
const STTY_FILE: &str = "-f";
#[cfg(not(target_os = "macos"))]
const STTY_FILE: &str = "-F";

/// BSD `ps` takes `-axo`; procps wants `-eo` and spells the column `args`.
#[cfg(target_os = "macos")]
const PS_PROCESSES: [&str; 2] = ["-axo", "pid=,ppid=,tty=,%cpu=,rss=,etime=,command="];
#[cfg(not(target_os = "macos"))]
const PS_PROCESSES: [&str; 2] = ["-eo", "pid=,ppid=,tty=,%cpu=,rss=,etime=,args="];

#[cfg(target_os = "macos")]
const PS_COMMANDS: [&str; 2] = ["-axo", "command="];
/// Ghostty only reports which tab is focused through its AppleScript
/// dictionary, which exists on macOS alone. Without it there is no way to tell
/// tabs apart, so focused-time tracking is a macOS feature.
const TRACKS_FOCUS: bool = cfg!(target_os = "macos");

/// Said once, wherever focused-time tracking is asked for and cannot be given.
const FOCUS_UNAVAILABLE: &str =
    "tracking which tab is focused needs macOS; Ghostty exposes no way to ask on this platform";

/// A process attached to a terminal. `ps` writes no tty as `??` on macOS and
/// `?` on Linux.
fn has_tty(tty: &str) -> bool {
    !tty.is_empty() && tty != "??" && tty != "?"
}

/// The process that owns a terminal surface. macOS Ghostty starts each surface
/// through `/usr/bin/login`; on Linux it execs the shell straight away, so any
/// child of Ghostty holding a tty is a surface.
#[cfg(target_os = "macos")]
fn is_terminal_root(process: &Process) -> bool {
    is_login_process(&process.command)
}
#[cfg(not(target_os = "macos"))]
fn is_terminal_root(_process: &Process) -> bool {
    true
}

#[derive(Clone, Copy)]
enum ServiceAction {
    Install,
    Uninstall,
    Status,
}

#[derive(Clone, Copy)]
enum DemoHistoryAction {
    Seed,
    Clear,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Key {
    Char(u8),
    Up,
    Down,
    Home,
    End,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct MouseEvent {
    button: u16,
    column: usize,
    row: usize,
    pressed: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum InputEvent {
    Key(Key),
    Mouse(MouseEvent),
}

#[derive(Clone, Debug, Default)]
struct Layout {
    terminal_start_row: usize,
    shown_start: usize,
    shown_count: usize,
    calendar_cells: Vec<CalendarCell>,
    zones: Vec<ClickZone>,
}

#[derive(Clone, Debug)]
struct CalendarCell {
    start_column: usize,
    end_column: usize,
    row: usize,
    day: String,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum ClickAction {
    Sort(SortBy),
    SelectPrevious,
    SelectNext,
    ToggleExpand,
    ShowUsage,
    ShowMonitor,
    Quit,
    SetScale(UsageScale),
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct ClickZone {
    start_column: usize,
    end_column: usize,
    row: usize,
    action: ClickAction,
}

impl ClickZone {
    fn contains(&self, mouse: &MouseEvent) -> bool {
        mouse.row == self.row
            && mouse.column >= self.start_column
            && mouse.column <= self.end_column
    }
}

#[derive(Default)]
struct InputDecoder {
    pending: Vec<u8>,
}

#[derive(Clone, Debug)]
struct Process {
    pid: i32,
    ppid: i32,
    tty: String,
    cpu: f64,
    rss_kib: u64,
    elapsed: String,
    age_seconds: u64,
    command: String,
}

/// Tab names resolved off the render thread: the Ghostty query costs hundreds
/// of milliseconds, far too long to block input handling on.
#[derive(Default)]
struct LabelCache {
    by_tty: HashMap<String, String>,
    pending: Option<mpsc::Receiver<HashMap<String, String>>>,
    next_refresh: Option<Instant>,
}

impl LabelCache {
    fn poll(&mut self, now: Instant, shells: Vec<(String, i32)>) {
        if let Some(pending) = &self.pending {
            match pending.try_recv() {
                Ok(labels) => self.by_tty = labels,
                // A worker that died leaves the previous labels in place.
                Err(mpsc::TryRecvError::Disconnected) => {}
                Err(mpsc::TryRecvError::Empty) => return,
            }
            self.pending = None;
            self.next_refresh = Some(now + TAB_LABEL_REFRESH);
        }
        if shells.is_empty() || self.next_refresh.is_some_and(|next| now < next) {
            return;
        }
        let (sender, receiver) = mpsc::channel();
        thread::spawn(move || {
            let _ = sender.send(resolve_tab_labels(&shells));
        });
        self.pending = Some(receiver);
    }

    fn label_for(&self, tty: &str) -> String {
        self.by_tty
            .get(tty)
            .cloned()
            .unwrap_or_else(|| tty.to_string())
    }
}

#[derive(Clone, Debug, Default)]
struct Terminal {
    root_pid: i32,
    tty: String,
    /// The Ghostty tab name when it can be resolved, else the working
    /// directory, else the tty.
    label: String,
    cpu: f64,
    rss_kib: u64,
    processes: Vec<Process>,
    activity: String,
    elapsed: String,
    age_seconds: u64,
    memory_growth_kib: i64,
    memory_slope_kib_per_min: f64,
    memory_samples: usize,
    leak_suspected: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum SortBy {
    Cpu,
    Memory,
    Trend,
    Processes,
    Age,
    Activity,
    Tab,
}

#[derive(Clone, Copy, PartialEq)]
enum View {
    Monitor,
    Usage,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum UsageScale {
    Daily,
    Weekly,
    Cumulative,
}

impl UsageScale {
    const ALL: [Self; 3] = [Self::Daily, Self::Weekly, Self::Cumulative];

    fn label(self) -> &'static str {
        match self {
            Self::Daily => "daily",
            Self::Weekly => "weekly",
            Self::Cumulative => "cumulative",
        }
    }

    /// Smallest value that still counts as a full-brightness cell, so a quiet
    /// stretch of days does not light up the grid as if it were a busy one.
    fn brightness_floor(self) -> u64 {
        match self {
            Self::Daily => 4 * 3_600,
            Self::Weekly => 20 * 3_600,
            Self::Cumulative => 0,
        }
    }
}

#[derive(Default)]
struct MemoryHistory {
    samples: VecDeque<(Instant, u64)>,
    process_ids: Vec<i32>,
    suspicious_windows: u8,
    clear_windows: u8,
    alert_active: bool,
}

#[derive(Clone, Copy, Debug, Default)]
struct MemoryTrend {
    growth_kib: i64,
    slope_kib_per_min: f64,
    suspected: bool,
}

#[derive(Clone, Debug, Default, PartialEq)]
struct TabUsage {
    name: String,
    seconds: u64,
}

#[derive(Default)]
struct UsageStore {
    path: Option<PathBuf>,
    days: BTreeMap<String, BTreeMap<String, TabUsage>>,
    dirty: bool,
}

#[derive(Clone, Debug, PartialEq)]
struct FocusedTab {
    tab_id: String,
    tab_name: String,
    terminal_id: String,
    terminal_name: String,
}

#[derive(Clone, Debug, PartialEq)]
struct TabObservation {
    app_frontmost: bool,
    window_id: String,
    window_name: String,
    tab_id: String,
    tab_name: String,
    tab_index: u32,
    selected: bool,
    terminal_count: u32,
    terminal_id: String,
    terminal_name: String,
    working_directory: String,
    terminal_focused: bool,
}

struct UsageTracker {
    store: UsageStore,
    current_day: String,
    last_focus: Option<FocusedTab>,
    /// Monotonic and wall-clock stamps of the last observation, so a gap can be
    /// recognised as a sleep whichever clock the system froze.
    last_seen: Option<(Instant, SystemTime)>,
    away: Option<Away>,
    next_poll: Instant,
    next_date_check: Instant,
    next_flush: Instant,
    next_inventory: Instant,
    next_inventory_heartbeat: Instant,
    next_resource_history: Instant,
    last_inventory: Vec<TabObservation>,
    pending_focus: Option<mpsc::Receiver<Result<FocusUpdate, String>>>,
    pending_inventory: Option<mpsc::Receiver<Result<Vec<TabObservation>, String>>>,
    last_error: Option<String>,
}

struct App {
    terminals: Vec<Terminal>,
    ghostty: Option<Process>,
    selected: usize,
    sort: SortBy,
    descending: bool,
    expanded: bool,
    interval: Duration,
    last_error: Option<String>,
    memory_history: HashMap<i32, MemoryHistory>,
    labels: LabelCache,
    usage_tracker: Option<UsageTracker>,
    view: View,
    tracker_outdated: bool,
    usage_scale: UsageScale,
    hovered_day: Option<String>,
    selected_day: Option<String>,
}

impl App {
    fn new(interval: Duration) -> Self {
        Self {
            terminals: Vec::new(),
            ghostty: None,
            selected: 0,
            sort: SortBy::Memory,
            descending: true,
            expanded: false,
            interval,
            last_error: None,
            memory_history: HashMap::new(),
            labels: LabelCache::default(),
            usage_tracker: None,
            view: View::Monitor,
            tracker_outdated: false,
            usage_scale: UsageScale::Daily,
            hovered_day: None,
            selected_day: None,
        }
    }

    fn refresh(&mut self) {
        match sample_processes() {
            Ok(processes) => {
                let selected_tty = self.terminals.get(self.selected).map(|t| t.tty.clone());
                let (ghostty, mut terminals) = aggregate(&processes);
                for terminal in &mut terminals {
                    terminal.label = self.labels.label_for(&terminal.tty);
                }
                self.update_memory_trends(&mut terminals, Instant::now());
                sort_terminals(&mut terminals, self.sort, self.descending);
                self.ghostty = ghostty;
                self.terminals = terminals;
                self.selected = selected_tty
                    .and_then(|tty| self.terminals.iter().position(|t| t.tty == tty))
                    .unwrap_or(self.selected.min(self.terminals.len().saturating_sub(1)));
                if let Some(tracker) = &mut self.usage_tracker {
                    tracker.capture_resources(
                        Instant::now(),
                        &self.terminals,
                        self.ghostty.as_ref(),
                    );
                }
                self.last_error = None;
            }
            Err(error) => self.last_error = Some(error),
        }
    }

    fn handle_key(&mut self, key: Key) -> bool {
        // The calendar reuses letters that sort columns in the monitor view, so
        // claim them here before the shared bindings see them.
        if self.view == View::Usage {
            match key {
                Key::Char(b'd') => {
                    self.usage_scale = UsageScale::Daily;
                    return true;
                }
                Key::Char(b'w') => {
                    self.usage_scale = UsageScale::Weekly;
                    return true;
                }
                Key::Char(b'c') => {
                    self.usage_scale = UsageScale::Cumulative;
                    return true;
                }
                _ => {}
            }
        }
        match key {
            Key::Char(b'q' | 3) => return false,
            Key::Char(b'c') => self.set_sort(SortBy::Cpu),
            Key::Char(b'm') => self.set_sort(SortBy::Memory),
            Key::Char(b'g') => self.set_sort(SortBy::Trend),
            Key::Char(b'p') => self.set_sort(SortBy::Processes),
            Key::Char(b'a') => self.set_sort(SortBy::Age),
            Key::Char(b'n') => self.set_sort(SortBy::Activity),
            Key::Char(b't') => self.set_sort(SortBy::Tab),
            Key::Char(b'u') => {
                self.view = if self.view == View::Monitor {
                    View::Usage
                } else {
                    View::Monitor
                };
            }
            Key::Char(b'r') => {
                self.descending = !self.descending;
                sort_terminals(&mut self.terminals, self.sort, self.descending);
            }
            Key::Char(b'j') | Key::Down => self.move_selection(1),
            Key::Char(b'k') | Key::Up => self.move_selection(-1),
            Key::Home => self.selected = 0,
            Key::End => self.selected = self.terminals.len().saturating_sub(1),
            Key::Char(b'e' | b'\n' | b' ') => self.expanded = !self.expanded,
            _ => {}
        }
        true
    }

    fn handle_mouse(&mut self, mouse: MouseEvent, layout: &Layout) -> bool {
        if self.view == View::Usage {
            self.hovered_day = layout
                .calendar_cells
                .iter()
                .find(|cell| {
                    mouse.row == cell.row
                        && mouse.column >= cell.start_column
                        && mouse.column <= cell.end_column
                })
                .map(|cell| cell.day.clone());
            if mouse.pressed && mouse.button == 0 {
                if let Some(day) = &self.hovered_day {
                    self.selected_day = Some(day.clone());
                } else if let Some(zone) = layout.zones.iter().find(|zone| zone.contains(&mouse)) {
                    return self.apply(zone.action);
                }
            }
            return true;
        }
        if !mouse.pressed {
            return true;
        }
        match mouse.button {
            64 => self.move_selection(-3),
            65 => self.move_selection(3),
            0 => {
                if mouse.row >= layout.terminal_start_row
                    && mouse.row < layout.terminal_start_row + layout.shown_count
                {
                    let clicked = layout.shown_start + mouse.row - layout.terminal_start_row;
                    if clicked == self.selected {
                        self.expanded = !self.expanded;
                    } else {
                        self.selected = clicked;
                    }
                } else if let Some(zone) = layout.zones.iter().find(|zone| zone.contains(&mouse)) {
                    return self.apply(zone.action);
                }
            }
            _ => {}
        }
        true
    }

    /// Runs a click target's action, returning false when the app should exit.
    fn apply(&mut self, action: ClickAction) -> bool {
        match action {
            ClickAction::Sort(sort) => self.set_sort(sort),
            ClickAction::SelectPrevious => self.move_selection(-1),
            ClickAction::SelectNext => self.move_selection(1),
            ClickAction::ToggleExpand => self.expanded = !self.expanded,
            ClickAction::ShowUsage => self.view = View::Usage,
            ClickAction::ShowMonitor => self.view = View::Monitor,
            ClickAction::SetScale(scale) => self.usage_scale = scale,
            ClickAction::Quit => return false,
        }
        true
    }

    fn move_selection(&mut self, offset: isize) {
        let last = self.terminals.len().saturating_sub(1) as isize;
        self.selected = (self.selected as isize + offset).clamp(0, last) as usize;
    }

    fn set_sort(&mut self, sort: SortBy) {
        if self.sort == sort {
            self.descending = !self.descending;
        } else {
            self.sort = sort;
            self.descending = sort != SortBy::Tab;
        }
        sort_terminals(&mut self.terminals, self.sort, self.descending);
    }

    fn update_memory_trends(&mut self, terminals: &mut [Terminal], now: Instant) {
        self.memory_history.retain(|root_pid, _| {
            terminals
                .iter()
                .any(|terminal| terminal.root_pid == *root_pid)
        });
        for terminal in terminals {
            let history = self.memory_history.entry(terminal.root_pid).or_default();
            let mut process_ids: Vec<i32> = terminal
                .processes
                .iter()
                .map(|process| process.pid)
                .collect();
            process_ids.sort_unstable();
            if history.process_ids != process_ids {
                history.samples.clear();
                history.suspicious_windows = 0;
                history.clear_windows = 0;
                history.alert_active = false;
                history.process_ids = process_ids;
            }
            history.samples.push_back((now, terminal.rss_kib));
            while history
                .samples
                .front()
                .is_some_and(|(time, _)| now.duration_since(*time) > HISTORY_WINDOW)
            {
                history.samples.pop_front();
            }
            let trend = analyze_memory_history(&history.samples);
            terminal.memory_growth_kib = trend.growth_kib;
            terminal.memory_slope_kib_per_min = trend.slope_kib_per_min;
            terminal.memory_samples = history.samples.len();
            terminal.leak_suspected = update_leak_state(history, trend.suspected);
        }
    }

    /// Kicks off a tab-name refresh when the cached names are stale.
    fn poll_labels(&mut self, now: Instant) {
        let shells = self
            .terminals
            .iter()
            .map(|terminal| (terminal.tty.clone(), shell_pid(terminal)))
            .collect();
        self.labels.poll(now, shells);
    }

    fn enable_usage_tracking(&mut self) {
        self.usage_tracker = Some(UsageTracker::load(Instant::now()));
    }

    fn track_usage(&mut self, now: Instant) -> bool {
        self.usage_tracker
            .as_mut()
            .is_some_and(|tracker| tracker.tick(now))
    }

    fn flush_usage(&mut self) {
        if let Some(tracker) = &mut self.usage_tracker {
            let _ = tracker.store.save();
        }
    }
}

impl UsageStore {
    fn load(path: Option<PathBuf>) -> Self {
        let mut store = Self {
            path,
            ..Self::default()
        };
        let Some(path) = &store.path else {
            return store;
        };
        let Ok(contents) = fs::read_to_string(path) else {
            return store;
        };
        for line in contents.lines() {
            let mut fields = line.splitn(4, '\t');
            let (Some(day), Some(tab_id), Some(seconds), Some(name)) =
                (fields.next(), fields.next(), fields.next(), fields.next())
            else {
                continue;
            };
            let Ok(seconds) = seconds.parse::<u64>() else {
                continue;
            };
            let seconds = seconds.min(MAX_DAILY_SECONDS);
            store.days.entry(day.to_string()).or_default().insert(
                tab_id.to_string(),
                TabUsage {
                    name: name.to_string(),
                    seconds,
                },
            );
        }
        store
    }

    fn add(&mut self, day: &str, focus: &FocusedTab, seconds: u64) {
        if seconds == 0 {
            return;
        }
        let entry = self
            .days
            .entry(day.to_string())
            .or_default()
            .entry(focus.tab_id.clone())
            .or_default();
        entry.name = display_tab_name(focus);
        entry.seconds = entry.seconds.saturating_add(seconds).min(MAX_DAILY_SECONDS);
        self.dirty = true;
    }

    fn save(&mut self) -> io::Result<()> {
        if !self.dirty {
            return Ok(());
        }
        let Some(path) = &self.path else {
            self.dirty = false;
            return Ok(());
        };
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut contents = String::new();
        for (day, tabs) in &self.days {
            for (tab_id, usage) in tabs {
                contents.push_str(&format!(
                    "{}\t{}\t{}\t{}\n",
                    clean_field(day),
                    clean_field(tab_id),
                    usage.seconds,
                    clean_field(&usage.name)
                ));
            }
        }
        let temporary = path.with_extension("tmp");
        fs::write(&temporary, contents)?;
        fs::rename(temporary, path)?;
        self.dirty = false;
        Ok(())
    }
}

impl UsageTracker {
    fn load(now: Instant) -> Self {
        let mut tracker = Self {
            store: UsageStore::load(default_usage_path()),
            current_day: local_date().unwrap_or_else(|| "unknown".to_string()),
            last_focus: None,
            last_seen: None,
            away: None,
            next_poll: now,
            next_date_check: now + Duration::from_secs(60),
            next_flush: now + USAGE_FLUSH_INTERVAL,
            next_inventory: now,
            next_inventory_heartbeat: now,
            next_resource_history: now,
            last_inventory: Vec::new(),
            pending_focus: None,
            pending_inventory: None,
            last_error: None,
        };
        if let Err(error) = append_tracker_event("start") {
            tracker.last_error = Some(format!("could not save tracker event: {error}"));
        }
        tracker
    }

    fn tick(&mut self, now: Instant) -> bool {
        if !TRACKS_FOCUS {
            return false;
        }
        if now >= self.next_date_check {
            if let Some(day) = local_date() {
                self.current_day = day;
            }
            self.next_date_check = now + Duration::from_secs(60);
        }
        let changed = self.poll_focus(now);
        self.poll_inventory(now);
        if now >= self.next_flush {
            if let Err(error) = self.store.save() {
                self.last_error = Some(format!("could not save usage: {error}"));
            }
            self.next_flush = now + USAGE_FLUSH_INTERVAL;
        }
        changed
    }

    /// Credits the time since the previous sample and starts the next one.
    /// Everything that can block — the presence probes and the Ghostty query —
    /// happens on the worker, so the UI thread only ever moves counters.
    fn poll_focus(&mut self, now: Instant) -> bool {
        let mut changed = false;
        if let Some(pending) = &self.pending_focus {
            match pending.try_recv() {
                Ok(Ok(FocusUpdate::Away(away))) => {
                    self.away = Some(away);
                    self.forget_focus();
                    self.next_poll = now + USAGE_POLL_INTERVAL;
                }
                Ok(Ok(FocusUpdate::Sample { focus, at, wall })) => {
                    self.away = None;
                    if let (Some(previous), Some(seen), Some(current)) =
                        (&self.last_focus, self.last_seen, &focus)
                    {
                        if previous.tab_id == current.tab_id {
                            if let Some(elapsed) = credited_time(seen, (at, wall)) {
                                self.store
                                    .add(&self.current_day, current, elapsed.as_secs());
                                changed = true;
                            }
                        }
                    }
                    self.last_focus = focus;
                    self.last_seen = Some((at, wall));
                    self.last_error = None;
                    self.next_poll = now + USAGE_POLL_INTERVAL;
                }
                Ok(Err(error)) => {
                    self.forget_focus();
                    self.last_error = Some(error);
                    self.next_poll = now + Duration::from_secs(30);
                }
                Err(mpsc::TryRecvError::Empty) => return false,
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.next_poll = now + USAGE_POLL_INTERVAL;
                }
            }
            self.pending_focus = None;
        }
        if now < self.next_poll {
            return changed;
        }
        let (sender, receiver) = mpsc::channel();
        thread::spawn(move || {
            let update = match away_reason() {
                Some(away) => Ok(FocusUpdate::Away(away)),
                None => query_focused_tab().map(|focus| FocusUpdate::Sample {
                    focus,
                    at: Instant::now(),
                    wall: SystemTime::now(),
                }),
            };
            let _ = sender.send(update);
        });
        self.pending_focus = Some(receiver);
        changed
    }

    /// Collects a finished tab-history snapshot and starts the next one. The
    /// query is slow enough that running it inline would visibly stall the UI.
    fn poll_inventory(&mut self, now: Instant) {
        if let Some(pending) = &self.pending_inventory {
            match pending.try_recv() {
                Ok(Ok(inventory)) => {
                    if inventory != self.last_inventory || now >= self.next_inventory_heartbeat {
                        if let Err(error) = append_tab_inventory(&self.current_day, &inventory) {
                            self.last_error = Some(format!("could not save tab history: {error}"));
                        }
                        self.last_inventory = inventory;
                        self.next_inventory_heartbeat = now + INVENTORY_HEARTBEAT;
                    }
                }
                Ok(Err(error)) => self.last_error = Some(error),
                Err(mpsc::TryRecvError::Empty) => return,
                Err(mpsc::TryRecvError::Disconnected) => {}
            }
            self.pending_inventory = None;
            self.next_inventory = now + INVENTORY_POLL_INTERVAL;
        }
        if now < self.next_inventory {
            return;
        }
        let (sender, receiver) = mpsc::channel();
        thread::spawn(move || {
            let _ = sender.send(query_tab_inventory());
        });
        self.pending_inventory = Some(receiver);
    }

    fn forget_focus(&mut self) {
        self.last_focus = None;
        self.last_seen = None;
    }

    fn capture_resources(
        &mut self,
        now: Instant,
        terminals: &[Terminal],
        ghostty: Option<&Process>,
    ) {
        if now < self.next_resource_history {
            return;
        }
        if let Err(error) = append_resource_history(&self.current_day, terminals, ghostty) {
            self.last_error = Some(format!("could not save resource history: {error}"));
        }
        self.next_resource_history = now + RESOURCE_HISTORY_INTERVAL;
    }
}

/// Resolves each terminal's tty to a name a human recognises. Ghostty's
/// scripting dictionary exposes no tty, so a surface is matched to a tty by
/// working directory; when a directory holds more than one differently-named
/// tab the match is ambiguous and the directory itself is shown instead.
fn resolve_tab_labels(shells: &[(String, i32)]) -> HashMap<String, String> {
    let pids: Vec<i32> = shells.iter().map(|(_, pid)| *pid).collect();
    let directories = shell_working_directories(&pids);
    let tabs = query_tab_names().unwrap_or_default();

    let mut surfaces_by_directory: HashMap<&str, Vec<&str>> = HashMap::new();
    for (name, directory) in &tabs {
        surfaces_by_directory
            .entry(directory.as_str())
            .or_default()
            .push(name.trim());
    }

    shells
        .iter()
        .filter_map(|(tty, pid)| {
            let directory = directories.get(pid)?;
            // A name is only trustworthy when one surface owns the directory.
            // With several, nothing distinguishes them and a guess would put
            // the wrong name on a row, which is worse than showing the path.
            let label = match surfaces_by_directory
                .get(directory.as_str())
                .map(Vec::as_slice)
            {
                Some([only]) if !only.is_empty() => clean_tab_name(only),
                _ => shorten_path(directory),
            };
            (!label.is_empty()).then(|| (tty.clone(), label))
        })
        .collect()
}

/// Tab names paired with each surface's working directory. Deliberately leaner
/// than the telemetry inventory because it runs every few seconds.
#[cfg(not(target_os = "macos"))]
fn query_tab_names() -> Result<Vec<(String, String)>, String> {
    // No scripting interface outside macOS, so rows fall back to directories.
    Ok(Vec::new())
}

#[cfg(target_os = "macos")]
fn query_tab_names() -> Result<Vec<(String, String)>, String> {
    if !ghostty_is_running() {
        return Ok(Vec::new());
    }
    const SCRIPT: &str = r#"tell application "Ghostty"
set fieldSeparator to ASCII character 31
set recordSeparator to ASCII character 30
set resultText to ""
repeat with ghosttyWindow in windows
repeat with ghosttyTab in tabs of ghosttyWindow
set tabName to name of ghosttyTab as text
repeat with ghosttyTerminal in terminals of ghosttyTab
set resultText to resultText & tabName & fieldSeparator & (working directory of ghosttyTerminal as text) & recordSeparator
end repeat
end repeat
end repeat
return resultText
end tell"#;
    let output = run_osascript(SCRIPT, INVENTORY_TIMEOUT)?;
    Ok(output
        .trim_end()
        .split('\x1e')
        .filter(|record| !record.is_empty())
        .filter_map(|record| {
            let (name, directory) = record.split_once('\x1f')?;
            Some((name.to_string(), directory.to_string()))
        })
        .collect())
}

#[cfg(not(target_os = "macos"))]
fn shell_working_directories(pids: &[i32]) -> HashMap<i32, String> {
    // /proc answers this directly, with no process to spawn.
    pids.iter()
        .filter_map(|pid| {
            let target = fs::read_link(format!("/proc/{pid}/cwd")).ok()?;
            Some((*pid, target.to_string_lossy().into_owned()))
        })
        .collect()
}

#[cfg(target_os = "macos")]
fn shell_working_directories(pids: &[i32]) -> HashMap<i32, String> {
    if pids.is_empty() {
        return HashMap::new();
    }
    let list = pids
        .iter()
        .map(i32::to_string)
        .collect::<Vec<_>>()
        .join(",");
    let Ok(output) = Command::new("lsof")
        .args(["-a", "-d", "cwd", "-p", &list, "-Fpn"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
    else {
        return HashMap::new();
    };
    let text = String::from_utf8_lossy(&output.stdout);
    let mut directories = HashMap::new();
    let mut current = None;
    for line in text.lines() {
        if let Some(pid) = line.strip_prefix('p') {
            current = pid.parse::<i32>().ok();
        } else if let Some(path) = line.strip_prefix('n') {
            if let Some(pid) = current {
                directories.insert(pid, path.to_string());
            }
        }
    }
    directories
}

/// Shells title their tab `user@host:~/path`, where only the path is worth
/// showing. Anything the user named themselves is kept verbatim.
fn clean_tab_name(name: &str) -> String {
    match name.split_once(':') {
        Some((owner, path)) if owner.contains('@') && !path.trim().is_empty() => {
            path.trim().to_string()
        }
        _ => name.trim().to_string(),
    }
}

fn shorten_path(path: &str) -> String {
    let Some(home) = env::var_os("HOME") else {
        return path.to_string();
    };
    let home = home.to_string_lossy();
    match path.strip_prefix(home.as_ref()) {
        Some("") => "~".to_string(),
        Some(rest) if rest.starts_with('/') => format!("~{rest}"),
        _ => path.to_string(),
    }
}

/// The interactive shell for a terminal, whose working directory is the one
/// Ghostty reports for the matching surface.
fn shell_pid(terminal: &Terminal) -> i32 {
    terminal
        .processes
        .iter()
        .find(|process| process.ppid == terminal.root_pid && is_shell(&process.command))
        .or_else(|| {
            terminal
                .processes
                .iter()
                .find(|process| process.ppid == terminal.root_pid)
        })
        .map_or(terminal.root_pid, |process| process.pid)
}

/// Time to credit between two observations, or None when the gap is too large
/// to have been continuous use. Both clocks must agree: whether the platform
/// freezes the monotonic clock across a sleep or keeps counting through it, one
/// of the two readings exposes the gap and the sample is dropped.
fn credited_time(previous: (Instant, SystemTime), now: (Instant, SystemTime)) -> Option<Duration> {
    let limit = USAGE_POLL_INTERVAL * 3;
    let elapsed = now.0.duration_since(previous.0);
    // A backwards wall clock yields an error here, which also fails the check.
    let wall_elapsed = now.1.duration_since(previous.1).ok()?;
    (elapsed <= limit && wall_elapsed <= limit).then_some(elapsed)
}

/// One reading from the focus worker.
enum FocusUpdate {
    Away(Away),
    Sample {
        focus: Option<FocusedTab>,
        at: Instant,
        wall: SystemTime,
    },
}

/// Why focused time is not being counted right now.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
#[derive(Clone, Copy, Debug, PartialEq)]
enum Away {
    Idle,
    LidClosed,
}

impl Away {
    fn reason(self) -> &'static str {
        match self {
            Self::Idle => "you stepped away",
            Self::LidClosed => "the lid is closed",
        }
    }
}

/// Detects the states where the user is not actually at the machine. System
/// sleep needs no check here: the elapsed-time guard in `tick` discards the
/// gap, because a sleeping Mac cannot poll in the first place.
#[cfg(not(target_os = "macos"))]
fn away_reason() -> Option<Away> {
    None
}

#[cfg(target_os = "macos")]
fn away_reason() -> Option<Away> {
    if lid_is_closed() {
        return Some(Away::LidClosed);
    }
    // Covers a locked screen, a sleeping display, and simply walking off.
    if hid_idle_time().is_some_and(|idle| idle > MAX_IDLE) {
        return Some(Away::Idle);
    }
    None
}

#[cfg(target_os = "macos")]
fn lid_is_closed() -> bool {
    // Desktop Macs have no clamshell key at all, so a missing value means open.
    ioreg_value(
        &["-r", "-k", "AppleClamshellState", "-d", "1"],
        "AppleClamshellState",
    )
    .is_some_and(|state| state.trim() == "Yes")
}

#[cfg(target_os = "macos")]
fn hid_idle_time() -> Option<Duration> {
    let nanoseconds: u64 = ioreg_value(&["-c", "IOHIDSystem", "-d", "4"], "HIDIdleTime")?
        .trim()
        .parse()
        .ok()?;
    Some(Duration::from_nanos(nanoseconds))
}

/// Reads `"<key>" = <value>` out of an ioreg dump.
#[cfg(target_os = "macos")]
fn ioreg_value(arguments: &[&str], key: &str) -> Option<String> {
    let output = Command::new("ioreg")
        .args(arguments)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    let needle = format!("\"{key}\" = ");
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .find_map(|line| Some(line.split_once(&needle)?.1.to_string()))
}

fn default_usage_path() -> Option<PathBuf> {
    default_data_dir().map(|directory| directory.join("usage.tsv"))
}

fn seed_demo_history() -> io::Result<usize> {
    let today = local_date().unwrap_or_else(|| "1970-01-01".to_string());
    seed_demo_history_at(default_usage_path(), &today)
}

fn seed_demo_history_at(path: Option<PathBuf>, today: &str) -> io::Result<usize> {
    const DEMO_TABS: [(&str, &str); 4] = [
        ("demo:editor", "Demo · editor"),
        ("demo:server", "Demo · server"),
        ("demo:tests", "Demo · tests"),
        ("demo:shell", "Demo · shell"),
    ];
    let today_number = date_to_day_number(today)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "invalid current date"))?;
    let mut store = UsageStore::load(path);
    let mut seeded_days = 0;
    for offset in 1_u64..=55 {
        if offset % 5 == 0 || offset % 13 == 0 {
            continue;
        }
        let day = day_number_to_date(today_number - offset as i64);
        let tab_count = 1 + (offset as usize % DEMO_TABS.len());
        let entries = store.days.entry(day).or_default();
        for (tab_index, (tab_id, tab_name)) in DEMO_TABS.iter().take(tab_count).enumerate() {
            let seconds = 15 * 60 + (offset * 977 + tab_index as u64 * 1_597) % (2 * 60 * 60);
            entries.insert(
                (*tab_id).to_string(),
                TabUsage {
                    name: (*tab_name).to_string(),
                    seconds,
                },
            );
        }
        seeded_days += 1;
    }
    store.dirty = true;
    store.save()?;
    Ok(seeded_days)
}

fn clear_demo_history() -> io::Result<usize> {
    clear_demo_history_at(default_usage_path())
}

fn clear_demo_history_at(path: Option<PathBuf>) -> io::Result<usize> {
    let mut store = UsageStore::load(path);
    let mut changed_days = 0;
    store.days.retain(|_, tabs| {
        let previous_len = tabs.len();
        tabs.retain(|tab_id, _| !tab_id.starts_with("demo:"));
        if tabs.len() != previous_len {
            changed_days += 1;
        }
        !tabs.is_empty()
    });
    if changed_days > 0 {
        store.dirty = true;
        store.save()?;
    }
    Ok(changed_days)
}

#[cfg(target_os = "macos")]
fn default_data_dir() -> Option<PathBuf> {
    env::var_os("HOME").map(|home| {
        PathBuf::from(home)
            .join("Library")
            .join("Application Support")
            .join("ghostty-top")
    })
}

#[cfg(not(target_os = "macos"))]
fn default_data_dir() -> Option<PathBuf> {
    let base = env::var_os("XDG_DATA_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            env::var_os("HOME").map(|home| PathBuf::from(home).join(".local").join("share"))
        })?;
    Some(base.join("ghostty-top"))
}

#[cfg(target_os = "macos")]
fn launch_agent_path() -> Option<PathBuf> {
    env::var_os("HOME").map(|home| {
        PathBuf::from(home)
            .join("Library")
            .join("LaunchAgents")
            .join(format!("{LAUNCH_AGENT_LABEL}.plist"))
    })
}

/// True when a login tracker is installed but is a different build than this
/// one. `--install-tracker` copies the binary to a fixed path, so upgrading
/// leaves that copy behind and the background tracker silently runs old code.
#[cfg(target_os = "macos")]
fn tracker_is_outdated() -> bool {
    let Some(installed) = default_data_dir().map(|dir| dir.join("bin").join("ghostty-top")) else {
        return false;
    };
    let Ok(current) = env::current_exe() else {
        return false;
    };
    match (fs::read(&installed), fs::read(&current)) {
        (Ok(installed), Ok(current)) => installed != current,
        // Nothing installed, or unreadable: there is nothing to warn about.
        _ => false,
    }
}

#[cfg(not(target_os = "macos"))]
fn install_login_tracker() -> Result<(), String> {
    Err(FOCUS_UNAVAILABLE.to_string())
}

#[cfg(not(target_os = "macos"))]
fn uninstall_login_tracker() -> Result<(), String> {
    Err(FOCUS_UNAVAILABLE.to_string())
}

#[cfg(not(target_os = "macos"))]
fn tracker_status() -> Result<(), String> {
    Err(FOCUS_UNAVAILABLE.to_string())
}

#[cfg(not(target_os = "macos"))]
fn tracker_is_outdated() -> bool {
    false
}

#[cfg(target_os = "macos")]
fn install_login_tracker() -> Result<(), String> {
    let data_dir = default_data_dir().ok_or("could not find the home directory")?;
    let agent_path = launch_agent_path().ok_or("could not find the LaunchAgents directory")?;
    let installed_binary = data_dir.join("bin").join("ghostty-top");
    let source_binary = env::current_exe()
        .map_err(|error| format!("could not locate this executable: {error}"))?
        .canonicalize()
        .map_err(|error| format!("could not resolve this executable: {error}"))?;

    fs::create_dir_all(installed_binary.parent().expect("binary has a parent"))
        .map_err(|error| format!("could not create tracker directory: {error}"))?;
    if source_binary != installed_binary {
        let temporary_binary = installed_binary.with_extension("new");
        fs::copy(&source_binary, &temporary_binary)
            .map_err(|error| format!("could not install tracker binary: {error}"))?;
        fs::rename(&temporary_binary, &installed_binary)
            .map_err(|error| format!("could not activate tracker binary: {error}"))?;
    }

    let log_path = data_dir.join("tracker.log");
    let error_log_path = data_dir.join("tracker-error.log");
    let plist = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>{LAUNCH_AGENT_LABEL}</string>
  <key>ProgramArguments</key>
  <array>
    <string>{}</string>
    <string>--track</string>
  </array>
  <key>RunAtLoad</key>
  <true/>
  <key>KeepAlive</key>
  <true/>
  <key>ProcessType</key>
  <string>Background</string>
  <key>ThrottleInterval</key>
  <integer>10</integer>
  <key>StandardOutPath</key>
  <string>{}</string>
  <key>StandardErrorPath</key>
  <string>{}</string>
</dict>
</plist>
"#,
        xml_escape(&installed_binary.to_string_lossy()),
        xml_escape(&log_path.to_string_lossy()),
        xml_escape(&error_log_path.to_string_lossy()),
    );
    let agent_parent = agent_path.parent().expect("launch agent has a parent");
    fs::create_dir_all(agent_parent)
        .map_err(|error| format!("could not create LaunchAgents directory: {error}"))?;
    let temporary_agent = agent_path.with_extension("plist.new");
    fs::write(&temporary_agent, plist)
        .map_err(|error| format!("could not write LaunchAgent: {error}"))?;
    fs::rename(&temporary_agent, &agent_path)
        .map_err(|error| format!("could not activate LaunchAgent file: {error}"))?;

    let domain = launchd_domain();
    let service = format!("{domain}/{LAUNCH_AGENT_LABEL}");
    let _ = Command::new("launchctl")
        .args(["bootout", &service])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    run_launchctl(["bootstrap", &domain, &agent_path.to_string_lossy()])?;
    run_launchctl(["enable", &service])?;
    run_launchctl(["kickstart", "-k", &service])?;
    println!("Installed and started {LAUNCH_AGENT_LABEL}.");
    println!(
        "Usage history remains at {}.",
        data_dir.join("usage.tsv").display()
    );
    Ok(())
}

#[cfg(target_os = "macos")]
fn uninstall_login_tracker() -> Result<(), String> {
    let data_dir = default_data_dir().ok_or("could not find the home directory")?;
    let agent_path = launch_agent_path().ok_or("could not find the LaunchAgents directory")?;
    let service = format!("{}/{LAUNCH_AGENT_LABEL}", launchd_domain());
    let _ = Command::new("launchctl")
        .args(["bootout", &service])
        .status();
    remove_if_present(&agent_path)?;
    remove_if_present(&data_dir.join("bin").join("ghostty-top"))?;
    println!("Removed the login tracker and installed tracker binary.");
    println!(
        "Usage history was preserved at {}.",
        data_dir.join("usage.tsv").display()
    );
    Ok(())
}

#[cfg(target_os = "macos")]
fn tracker_status() -> Result<(), String> {
    let service = format!("{}/{LAUNCH_AGENT_LABEL}", launchd_domain());
    let status = Command::new("launchctl")
        .args(["print", &service])
        .status()
        .map_err(|error| format!("could not query launchd: {error}"))?;
    if status.success() {
        Ok(())
    } else {
        Err("login tracker is not loaded; run --install-tracker".to_string())
    }
}

#[cfg(target_os = "macos")]
fn launchd_domain() -> String {
    format!("gui/{}", unsafe { getuid() })
}

#[cfg(target_os = "macos")]
fn run_launchctl<const N: usize>(arguments: [&str; N]) -> Result<(), String> {
    let status = Command::new("launchctl")
        .args(arguments)
        .status()
        .map_err(|error| format!("could not run launchctl: {error}"))?;
    status
        .success()
        .then_some(())
        .ok_or_else(|| format!("launchctl exited with {status}"))
}

#[cfg(target_os = "macos")]
fn remove_if_present(path: &PathBuf) -> Result<(), String> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("could not remove {}: {error}", path.display())),
    }
}

#[cfg(target_os = "macos")]
fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn display_tab_name(focus: &FocusedTab) -> String {
    if !focus.tab_name.trim().is_empty() {
        focus.tab_name.clone()
    } else if !focus.terminal_name.trim().is_empty() {
        focus.terminal_name.clone()
    } else {
        format!("Tab {}", truncate(&focus.tab_id, 8))
    }
}

fn clean_field(value: &str) -> String {
    value.replace(['\t', '\n', '\r'], " ")
}

fn local_date() -> Option<String> {
    let output = Command::new("date").arg("+%F").output().ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
}

#[cfg(not(target_os = "macos"))]
fn query_focused_tab() -> Result<Option<FocusedTab>, String> {
    Ok(None)
}

#[cfg(target_os = "macos")]
fn query_focused_tab() -> Result<Option<FocusedTab>, String> {
    if !ghostty_is_running() {
        return Ok(None);
    }
    const SCRIPT: &str = r#"tell application "Ghostty"
if not frontmost then return ""
set theWindow to front window
set theTab to selected tab of theWindow
set theTerminal to focused terminal of theTab
set separator to ASCII character 9
return (id of theTab as text) & separator & (name of theTab as text) & separator & (id of theTerminal as text) & separator & (name of theTerminal as text)
end tell"#;
    let output = run_osascript(SCRIPT, USAGE_SAMPLE_TIMEOUT)?;
    let output = output.trim();
    if output.is_empty() {
        return Ok(None);
    }
    let fields: Vec<&str> = output.splitn(4, '\t').collect();
    if fields.len() != 4 {
        return Err("Ghostty returned an unexpected focus response".to_string());
    }
    Ok(Some(FocusedTab {
        tab_id: fields[0].to_string(),
        tab_name: fields[1].to_string(),
        terminal_id: fields[2].to_string(),
        terminal_name: fields[3].to_string(),
    }))
}

#[cfg(not(target_os = "macos"))]
fn query_tab_inventory() -> Result<Vec<TabObservation>, String> {
    Ok(Vec::new())
}

#[cfg(target_os = "macos")]
fn query_tab_inventory() -> Result<Vec<TabObservation>, String> {
    if !ghostty_is_running() {
        return Ok(Vec::new());
    }
    const SCRIPT: &str = r#"tell application "Ghostty"
set fieldSeparator to ASCII character 31
set recordSeparator to ASCII character 30
set resultText to ""
set appIsFrontmost to frontmost
repeat with ghosttyWindow in windows
set windowID to id of ghosttyWindow as text
set windowName to name of ghosttyWindow as text
repeat with ghosttyTab in tabs of ghosttyWindow
    set tabID to id of ghosttyTab as text
set tabName to name of ghosttyTab as text
set tabIndex to index of ghosttyTab as text
set tabIsSelected to (selected of ghosttyTab) as text
set focusedID to id of focused terminal of ghosttyTab as text
set terminalCount to count of terminals of ghosttyTab
repeat with ghosttyTerminal in terminals of ghosttyTab
set terminalID to id of ghosttyTerminal as text
set terminalName to name of ghosttyTerminal as text
set terminalDirectory to working directory of ghosttyTerminal as text
set terminalIsFocused to (terminalID is focusedID) as text
set resultText to resultText & (appIsFrontmost as text) & fieldSeparator & windowID & fieldSeparator & windowName & fieldSeparator & tabID & fieldSeparator & tabName & fieldSeparator & tabIndex & fieldSeparator & tabIsSelected & fieldSeparator & (terminalCount as text) & fieldSeparator & terminalID & fieldSeparator & terminalName & fieldSeparator & terminalDirectory & fieldSeparator & terminalIsFocused & recordSeparator
end repeat
end repeat
end repeat
return resultText
end tell"#;
    // Walking every surface scales with tab count and routinely runs past a
    // second, so this query is given room and is never issued on the UI thread.
    let output = run_osascript(SCRIPT, INVENTORY_TIMEOUT)?;
    parse_tab_inventory(&output)
}

#[cfg(target_os = "macos")]
fn parse_tab_inventory(output: &str) -> Result<Vec<TabObservation>, String> {
    let mut observations = Vec::new();
    for record in output.trim_end().split('\x1e') {
        if record.is_empty() {
            continue;
        }
        let fields: Vec<&str> = record.split('\x1f').collect();
        if fields.len() != 12 {
            return Err("Ghostty returned an unexpected tab inventory".to_string());
        }
        observations.push(TabObservation {
            app_frontmost: fields[0] == "true",
            window_id: fields[1].to_string(),
            window_name: fields[2].to_string(),
            tab_id: fields[3].to_string(),
            tab_name: fields[4].to_string(),
            tab_index: fields[5].parse().unwrap_or(0),
            selected: fields[6] == "true",
            terminal_count: fields[7].parse().unwrap_or(0),
            terminal_id: fields[8].to_string(),
            terminal_name: fields[9].to_string(),
            working_directory: fields[10].to_string(),
            terminal_focused: fields[11] == "true",
        });
    }
    observations.sort_by(|a, b| {
        (&a.window_id, &a.tab_id, &a.terminal_id).cmp(&(&b.window_id, &b.tab_id, &b.terminal_id))
    });
    Ok(observations)
}

#[cfg(target_os = "macos")]
fn run_osascript(script: &str, timeout: Duration) -> Result<String, String> {
    let mut child = Command::new("osascript")
        .args(["-e", script])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| format!("could not start Ghostty tracking: {error}"))?;
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                if !status.success() {
                    return Err("Ghostty Automation permission is unavailable".to_string());
                }
                let mut output = String::new();
                if let Some(mut stdout) = child.stdout.take() {
                    stdout
                        .read_to_string(&mut output)
                        .map_err(|error| format!("could not read Ghostty state: {error}"))?;
                }
                return Ok(output);
            }
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(25)),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("Ghostty focus query timed out; allow Automation access".to_string());
            }
            Err(error) => return Err(format!("could not query focused tab: {error}")),
        }
    }
}

fn append_tab_inventory(day: &str, observations: &[TabObservation]) -> io::Result<()> {
    let Some(path) = default_data_dir().map(|directory| directory.join("tab-history-v1.tsv"))
    else {
        return Ok(());
    };
    let mut file = open_history_file(
        &path,
        "timestamp\tday\trecord\tapp_frontmost\twindow_id\twindow_name\ttab_id\ttab_name\ttab_index\tselected\tterminal_count\tterminal_id\tterminal_name\tworking_directory\tterminal_focused",
    )?;
    let timestamp = unix_timestamp();
    if observations.is_empty() {
        writeln!(file, "{timestamp}\t{}\tapp_closed", clean_field(day))?;
        return Ok(());
    }
    for value in observations {
        writeln!(
            file,
            "{timestamp}\t{}\tsnapshot\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
            clean_field(day),
            value.app_frontmost,
            clean_field(&value.window_id),
            clean_field(&value.window_name),
            clean_field(&value.tab_id),
            clean_field(&value.tab_name),
            value.tab_index,
            value.selected,
            value.terminal_count,
            clean_field(&value.terminal_id),
            clean_field(&value.terminal_name),
            clean_field(&value.working_directory),
            value.terminal_focused,
        )?;
    }
    Ok(())
}

fn append_resource_history(
    day: &str,
    terminals: &[Terminal],
    ghostty: Option<&Process>,
) -> io::Result<()> {
    let Some(path) = default_data_dir().map(|directory| directory.join("resources-v1.tsv")) else {
        return Ok(());
    };
    let mut file = open_history_file(
        &path,
        "timestamp\tday\tkind\tid\tcpu_percent\trss_kib\tprocesses\tage_seconds\tactivity\tmemory_growth_kib\tleak_suspected",
    )?;
    let timestamp = unix_timestamp();
    if let Some(process) = ghostty {
        writeln!(
            file,
            "{timestamp}\t{}\tapp\tghostty\t{:.1}\t{}\t1\t{}\tghostty\t0\tfalse",
            clean_field(day),
            process.cpu,
            process.rss_kib,
            process.age_seconds,
        )?;
    }
    for terminal in terminals {
        writeln!(
            file,
            "{timestamp}\t{}\tterminal\t{}\t{:.1}\t{}\t{}\t{}\t{}\t{}\t{}",
            clean_field(day),
            clean_field(&terminal.tty),
            terminal.cpu,
            terminal.rss_kib,
            terminal.processes.len(),
            terminal.age_seconds,
            clean_field(&terminal.activity),
            terminal.memory_growth_kib,
            terminal.leak_suspected,
        )?;
    }
    append_process_history(timestamp, day, terminals, ghostty)?;
    Ok(())
}

fn append_process_history(
    timestamp: u64,
    day: &str,
    terminals: &[Terminal],
    ghostty: Option<&Process>,
) -> io::Result<()> {
    let Some(path) = default_data_dir().map(|directory| directory.join("processes-v1.tsv")) else {
        return Ok(());
    };
    let mut file = open_history_file(
        &path,
        "timestamp\tday\tkind\tterminal_tty\tterminal_root_pid\tpid\tppid\tcpu_percent\trss_kib\tage_seconds\tstarted_at\texecutable_name\texecutable_path",
    )?;
    if let Some(process) = ghostty {
        writeln!(
            file,
            "{}",
            process_history_row(timestamp, day, "app", "", 0, process)
        )?;
    }
    for terminal in terminals {
        for process in &terminal.processes {
            writeln!(
                file,
                "{}",
                process_history_row(
                    timestamp,
                    day,
                    "terminal",
                    &terminal.tty,
                    terminal.root_pid,
                    process,
                )
            )?;
        }
    }
    Ok(())
}

fn process_history_row(
    timestamp: u64,
    day: &str,
    kind: &str,
    terminal_tty: &str,
    terminal_root_pid: i32,
    process: &Process,
) -> String {
    let executable_path = process.command.split_whitespace().next().unwrap_or("?");
    format!(
        "{timestamp}\t{}\t{}\t{}\t{terminal_root_pid}\t{}\t{}\t{:.1}\t{}\t{}\t{}\t{}\t{}",
        clean_field(day),
        clean_field(kind),
        clean_field(terminal_tty),
        process.pid,
        process.ppid,
        process.cpu,
        process.rss_kib,
        process.age_seconds,
        timestamp.saturating_sub(process.age_seconds),
        clean_field(&command_name(&process.command)),
        clean_field(executable_path),
    )
}

fn append_tracker_event(event: &str) -> io::Result<()> {
    let Some(path) = default_data_dir().map(|directory| directory.join("tracker-events-v1.tsv"))
    else {
        return Ok(());
    };
    let mut file = open_history_file(
        &path,
        "timestamp\tevent\tschema_version\tapp_version\tpid\tos\tarchitecture\tfocus_interval_seconds\tinventory_interval_seconds\tresource_interval_seconds",
    )?;
    writeln!(
        file,
        "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
        unix_timestamp(),
        clean_field(event),
        HISTORY_SCHEMA_VERSION,
        env!("CARGO_PKG_VERSION"),
        std::process::id(),
        env::consts::OS,
        env::consts::ARCH,
        USAGE_POLL_INTERVAL.as_secs(),
        INVENTORY_POLL_INTERVAL.as_secs(),
        RESOURCE_HISTORY_INTERVAL.as_secs(),
    )
}

fn open_history_file(path: &PathBuf, header: &str) -> io::Result<fs::File> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let needs_header = fs::metadata(path).map_or(true, |metadata| metadata.len() == 0);
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    if needs_header {
        writeln!(file, "{header}")?;
    }
    Ok(file)
}

fn unix_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// `pgrep -x` compares against the full executable path on macOS, so it never
/// matched the bundled binary and silently disabled every Ghostty query. The
/// process list is matched the same way `aggregate` does it instead.
#[cfg(target_os = "macos")]
fn ghostty_is_running() -> bool {
    let Ok(output) = Command::new("ps")
        .args(PS_COMMANDS)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
    else {
        return false;
    };
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .any(is_ghostty_app)
}

fn is_ghostty_app(command: &str) -> bool {
    let command = command.trim_end();
    command.ends_with("/ghostty") || command == "ghostty"
}

impl InputDecoder {
    fn push(&mut self, bytes: &[u8]) -> Vec<InputEvent> {
        self.pending.extend_from_slice(bytes);
        let mut events = Vec::new();
        loop {
            if self.pending.is_empty() {
                break;
            }
            if self.pending[0] != 0x1b {
                events.push(InputEvent::Key(Key::Char(self.pending.remove(0))));
                continue;
            }
            if self.pending.len() < 2 {
                break;
            }
            if self.pending.starts_with(b"\x1b[<") {
                let Some(end) = self
                    .pending
                    .iter()
                    .position(|byte| *byte == b'M' || *byte == b'm')
                else {
                    break;
                };
                let pressed = self.pending[end] == b'M';
                let body = String::from_utf8_lossy(&self.pending[3..end]);
                let values: Vec<u16> = body
                    .split(';')
                    .filter_map(|part| part.parse().ok())
                    .collect();
                if values.len() == 3 {
                    events.push(InputEvent::Mouse(MouseEvent {
                        button: values[0],
                        column: values[1] as usize,
                        row: values[2] as usize,
                        pressed,
                    }));
                }
                self.pending.drain(..=end);
                continue;
            }
            if self.pending.len() < 3 {
                break;
            }
            let key = match &self.pending[..3] {
                b"\x1b[A" => Some(Key::Up),
                b"\x1b[B" => Some(Key::Down),
                b"\x1b[H" => Some(Key::Home),
                b"\x1b[F" => Some(Key::End),
                _ => None,
            };
            if let Some(key) = key {
                events.push(InputEvent::Key(key));
                self.pending.drain(..3);
            } else {
                self.pending.remove(0);
            }
        }
        events
    }
}

fn main() {
    let mut once = false;
    let mut track_only = false;
    let mut service_action = None;
    let mut demo_history_action = None;
    let mut interval = Duration::from_secs(1);
    let args: Vec<String> = env::args().skip(1).collect();
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--once" => once = true,
            "--track" => track_only = true,
            "--install-tracker" => service_action = Some(ServiceAction::Install),
            "--uninstall-tracker" => service_action = Some(ServiceAction::Uninstall),
            "--tracker-status" => service_action = Some(ServiceAction::Status),
            "--seed-demo-history" => demo_history_action = Some(DemoHistoryAction::Seed),
            "--clear-demo-history" => demo_history_action = Some(DemoHistoryAction::Clear),
            "--interval" => {
                index += 1;
                let Some(value) = args.get(index) else {
                    usage_and_exit(2)
                };
                let Ok(seconds) = value.parse::<f64>() else {
                    usage_and_exit(2)
                };
                if !(0.2..=60.0).contains(&seconds) {
                    usage_and_exit(2)
                }
                interval = Duration::from_secs_f64(seconds);
            }
            "-V" | "--version" => {
                println!("ghostty-top {}", env!("CARGO_PKG_VERSION"));
                return;
            }
            "-h" | "--help" => usage_and_exit(0),
            _ => usage_and_exit(2),
        }
        index += 1;
    }

    if !cfg!(any(target_os = "macos", target_os = "linux")) {
        eprintln!("ghostty-top supports macOS and Linux.");
        std::process::exit(1);
    }

    if let Some(action) = service_action {
        let result = match action {
            ServiceAction::Install => install_login_tracker(),
            ServiceAction::Uninstall => uninstall_login_tracker(),
            ServiceAction::Status => tracker_status(),
        };
        if let Err(error) = result {
            eprintln!("Error: {error}");
            std::process::exit(1);
        }
        return;
    }

    if let Some(action) = demo_history_action {
        let result = match action {
            DemoHistoryAction::Seed => seed_demo_history(),
            DemoHistoryAction::Clear => clear_demo_history(),
        };
        match result {
            Ok(days) => println!(
                "{} demo history for {days} day(s).",
                if matches!(action, DemoHistoryAction::Seed) {
                    "Seeded"
                } else {
                    "Cleared"
                }
            ),
            Err(error) => {
                eprintln!("Error: {error}");
                std::process::exit(1);
            }
        }
        return;
    }

    let mut app = App::new(interval);
    app.refresh();

    if once {
        // One-shot output can afford to resolve tab names inline.
        let shells = app
            .terminals
            .iter()
            .map(|terminal| (terminal.tty.clone(), shell_pid(terminal)))
            .collect::<Vec<_>>();
        app.labels.by_tty = resolve_tab_labels(&shells);
        app.refresh();
        print_snapshot(&app);
        return;
    }

    app.enable_usage_tracking();
    app.tracker_outdated = tracker_is_outdated();

    if track_only {
        if !TRACKS_FOCUS {
            eprintln!("Error: {FOCUS_UNAVAILABLE}");
            std::process::exit(1);
        }
        println!("Tracking focused Ghostty tab usage. Press Ctrl-C to stop.");
        let mut next_resource_refresh = Instant::now();
        let mut reported_tracking_error: Option<String> = None;
        loop {
            let now = Instant::now();
            if now >= next_resource_refresh {
                app.refresh();
                next_resource_refresh = now + Duration::from_secs(60);
            }
            app.track_usage(now);
            let tracking_error = app
                .usage_tracker
                .as_ref()
                .and_then(|tracker| tracker.last_error.clone());
            if tracking_error != reported_tracking_error {
                match &tracking_error {
                    Some(error) => eprintln!("tracking warning: {error}"),
                    None if reported_tracking_error.is_some() => {
                        eprintln!("tracking recovered");
                    }
                    None => {}
                }
                reported_tracking_error = tracking_error;
            }
            thread::sleep(Duration::from_millis(200));
        }
    }

    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        eprintln!("Interactive mode needs a terminal. Use --once for plain output.");
        std::process::exit(1);
    }

    let Some(_terminal_guard) = TerminalGuard::enter() else {
        eprintln!("Could not configure the terminal. Try: ghostty-top --once");
        std::process::exit(1);
    };

    // Started before the first frame so names replace tty devices promptly
    // rather than after the first refresh tick.
    app.poll_labels(Instant::now());

    let mut stdin = io::stdin();
    let mut decoder = InputDecoder::default();
    let mut next_sample = Instant::now() + app.interval;
    let mut dirty = true;
    let mut layout = Layout::default();
    loop {
        if dirty {
            layout = render(&app);
            dirty = false;
        }
        let mut input = [0_u8; 64];
        if let Ok(count) = stdin.read(&mut input) {
            for event in decoder.push(&input[..count]) {
                let keep_running = match event {
                    InputEvent::Key(key) => app.handle_key(key),
                    InputEvent::Mouse(mouse) => app.handle_mouse(mouse, &layout),
                };
                if !keep_running {
                    app.flush_usage();
                    return;
                }
                dirty = true;
            }
        }
        if Instant::now() >= next_sample {
            app.poll_labels(Instant::now());
            app.refresh();
            next_sample = Instant::now() + app.interval;
            dirty = true;
        }
        if app.track_usage(Instant::now()) {
            dirty = true;
        }
        thread::sleep(Duration::from_millis(50));
    }
}

trait IsTerminal {
    fn is_terminal(&self) -> bool;
}

impl IsTerminal for io::Stdin {
    fn is_terminal(&self) -> bool {
        unsafe { isatty(0) == 1 }
    }
}

impl IsTerminal for io::Stdout {
    fn is_terminal(&self) -> bool {
        unsafe { isatty(1) == 1 }
    }
}

extern "C" {
    fn isatty(fd: i32) -> i32;
    #[cfg(target_os = "macos")]
    fn getuid() -> u32;
}

struct TerminalGuard {
    original: String,
}

impl TerminalGuard {
    fn enter() -> Option<Self> {
        let original = String::from_utf8(
            Command::new("stty")
                .args([STTY_FILE, "/dev/tty", "-g"])
                .output()
                .ok()?
                .stdout,
        )
        .ok()?
        .trim()
        .to_string();
        let ok = Command::new("stty")
            .args([
                STTY_FILE, "/dev/tty", "-echo", "-icanon", "-isig", "min", "0", "time", "0",
            ])
            .status()
            .ok()?
            .success();
        if !ok {
            return None;
        }
        print!("\x1b[?1049h\x1b[?25l\x1b[?1003h\x1b[?1006h");
        let _ = io::stdout().flush();
        Some(Self { original })
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = Command::new("stty")
            .args([STTY_FILE, "/dev/tty", &self.original])
            .status();
        print!("\x1b[?1003l\x1b[?1000l\x1b[?1006l\x1b[?25h\x1b[?1049l{RESET}");
        let _ = io::stdout().flush();
    }
}

fn usage_and_exit(code: i32) -> ! {
    eprintln!("Usage: ghostty-top [OPTIONS]\n\n  --once                print one process sample and exit\n  --track               track focused-tab usage without the TUI\n  --install-tracker     install and start tracking automatically at login\n  --uninstall-tracker   remove the login service but preserve usage history\n  --tracker-status      show the login tracker's launchd status\n  --seed-demo-history   add removable calendar test data for prior days\n  --clear-demo-history  remove demo data without touching real history\n  --interval SECONDS    process refresh rate from 0.2 to 60 (default: 1)\n  -V, --version         print the version and exit");
    std::process::exit(code);
}

fn sample_processes() -> Result<Vec<Process>, String> {
    let output = Command::new("ps")
        .args(PS_PROCESSES)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("could not run ps: {e}"))?;
    if !output.status.success() {
        return Err(format!("ps exited with {}", output.status));
    }
    let text = String::from_utf8_lossy(&output.stdout);
    Ok(text.lines().filter_map(parse_process).collect())
}

fn parse_process(line: &str) -> Option<Process> {
    let mut fields = line.split_whitespace();
    let pid = fields.next()?.parse().ok()?;
    let ppid = fields.next()?.parse().ok()?;
    let tty = fields.next()?.to_string();
    let cpu = fields.next()?.replace(',', ".").parse().ok()?;
    let rss_kib = fields.next()?.parse().ok()?;
    let elapsed = fields.next()?.to_string();
    let age_seconds = parse_elapsed(&elapsed)?;
    let command = fields.collect::<Vec<_>>().join(" ");
    Some(Process {
        pid,
        ppid,
        tty,
        cpu,
        rss_kib,
        elapsed,
        age_seconds,
        command,
    })
}

fn aggregate(processes: &[Process]) -> (Option<Process>, Vec<Terminal>) {
    let ghostty = processes
        .iter()
        .find(|p| is_ghostty_app(&p.command))
        .cloned();
    let Some(ref app) = ghostty else {
        return (None, Vec::new());
    };

    let roots: Vec<&Process> = processes
        .iter()
        .filter(|p| p.ppid == app.pid && has_tty(&p.tty) && is_terminal_root(p))
        .collect();
    let mut owner: HashMap<i32, i32> = roots.iter().map(|p| (p.pid, p.pid)).collect();
    let by_parent = child_map(processes);
    for root in &roots {
        assign_descendants(root.pid, root.pid, &by_parent, &mut owner);
    }

    let self_pid = std::process::id() as i32;
    let mut terminals = Vec::with_capacity(roots.len());
    for root in roots {
        let mut members: Vec<Process> = processes
            .iter()
            .filter(|p| owner.get(&p.pid) == Some(&root.pid))
            .filter(|p| {
                p.pid != self_pid && !(p.ppid == self_pid && command_name(&p.command) == "ps")
            })
            .cloned()
            .collect();
        let cpu = members.iter().map(|p| p.cpu).sum();
        let rss_kib = members.iter().map(|p| p.rss_kib).sum();
        members.sort_by_key(|process| std::cmp::Reverse(process.rss_kib));
        let activity = members
            .iter()
            .filter(|p| !is_login_process(&p.command) && !is_shell(&p.command))
            .max_by(|a, b| a.cpu.total_cmp(&b.cpu).then(a.rss_kib.cmp(&b.rss_kib)))
            .map(|p| command_name(&p.command))
            .unwrap_or_else(|| "shell".to_string());
        terminals.push(Terminal {
            root_pid: root.pid,
            label: root.tty.clone(),
            tty: root.tty.clone(),
            cpu,
            rss_kib,
            processes: members,
            activity,
            elapsed: root.elapsed.clone(),
            age_seconds: root.age_seconds,
            memory_growth_kib: 0,
            memory_slope_kib_per_min: 0.0,
            memory_samples: 0,
            leak_suspected: false,
        });
    }
    (ghostty, terminals)
}

fn child_map(processes: &[Process]) -> HashMap<i32, Vec<i32>> {
    let mut map: HashMap<i32, Vec<i32>> = HashMap::new();
    for process in processes {
        map.entry(process.ppid).or_default().push(process.pid);
    }
    map
}

fn assign_descendants(
    pid: i32,
    root: i32,
    children: &HashMap<i32, Vec<i32>>,
    owner: &mut HashMap<i32, i32>,
) {
    if let Some(child_pids) = children.get(&pid) {
        for child in child_pids {
            if owner.insert(*child, root).is_none() {
                assign_descendants(*child, root, children, owner);
            }
        }
    }
}

fn is_login_process(command: &str) -> bool {
    command.starts_with("/usr/bin/login ") || command == "/usr/bin/login"
}

fn is_shell(command: &str) -> bool {
    let name = command_name(command);
    matches!(name.as_str(), "zsh" | "bash" | "fish" | "sh" | "nu")
}

fn command_name(command: &str) -> String {
    let first = command.split_whitespace().next().unwrap_or("?");
    first
        .rsplit('/')
        .next()
        .unwrap_or(first)
        .trim_start_matches('-')
        .to_string()
}

fn parse_elapsed(value: &str) -> Option<u64> {
    let (days, clock) = if let Some((days, clock)) = value.split_once('-') {
        (days.parse().ok()?, clock)
    } else {
        (0, value)
    };
    let parts: Vec<u64> = clock
        .split(':')
        .map(str::parse)
        .collect::<Result<_, _>>()
        .ok()?;
    let clock_seconds = match parts.as_slice() {
        [minutes, seconds] => minutes * 60 + seconds,
        [hours, minutes, seconds] => hours * 3600 + minutes * 60 + seconds,
        _ => return None,
    };
    Some(days * 86_400 + clock_seconds)
}

fn analyze_memory_history(samples: &VecDeque<(Instant, u64)>) -> MemoryTrend {
    let Some((first_time, _)) = samples.front() else {
        return MemoryTrend::default();
    };
    let Some((last_time, _)) = samples.back() else {
        return MemoryTrend::default();
    };
    if samples.len() < 2 {
        return MemoryTrend::default();
    }
    let edge_count = (samples.len() / 5).clamp(1, 10);
    let baseline = samples
        .iter()
        .take(edge_count)
        .map(|(_, rss)| *rss)
        .sum::<u64>() as f64
        / edge_count as f64;
    let recent = samples
        .iter()
        .rev()
        .take(edge_count)
        .map(|(_, rss)| *rss)
        .sum::<u64>() as f64
        / edge_count as f64;
    let growth = recent - baseline;
    let duration = last_time.duration_since(*first_time);
    let ratio = if baseline > 0.0 {
        growth / baseline
    } else {
        0.0
    };
    let points: Vec<(f64, f64)> = samples
        .iter()
        .map(|(time, rss)| (time.duration_since(*first_time).as_secs_f64(), *rss as f64))
        .collect();
    let (slope_per_second, r_squared) = linear_regression(&points);
    let slope_kib_per_min = slope_per_second * 60.0;
    let recent_start = points.len() / 2;
    let (recent_slope_per_second, _) = linear_regression(&points[recent_start..]);
    let tolerance = (baseline * 0.01).max(2.0 * 1024.0);
    let upward_samples = samples
        .iter()
        .zip(samples.iter().skip(1))
        .filter(|((_, previous), (_, current))| *current as f64 + tolerance >= *previous as f64)
        .count();
    let upward_ratio = upward_samples as f64 / (samples.len() - 1) as f64;
    MemoryTrend {
        growth_kib: growth.round() as i64,
        slope_kib_per_min,
        suspected: samples.len() >= MIN_LEAK_SAMPLES
            && duration >= MIN_LEAK_DURATION
            && growth >= MIN_LEAK_GROWTH_KIB as f64
            && ratio >= MIN_LEAK_GROWTH_RATIO
            && slope_kib_per_min >= MIN_LEAK_SLOPE_KIB_PER_MIN
            && r_squared >= MIN_LEAK_R_SQUARED
            && recent_slope_per_second >= slope_per_second * 0.35
            && upward_ratio >= MIN_UPWARD_SAMPLE_RATIO,
    }
}

fn linear_regression(points: &[(f64, f64)]) -> (f64, f64) {
    if points.len() < 2 {
        return (0.0, 0.0);
    }
    let count = points.len() as f64;
    let mean_x = points.iter().map(|(x, _)| x).sum::<f64>() / count;
    let mean_y = points.iter().map(|(_, y)| y).sum::<f64>() / count;
    let mut sum_xx = 0.0;
    let mut sum_xy = 0.0;
    let mut sum_yy = 0.0;
    for (x, y) in points {
        let centered_x = x - mean_x;
        let centered_y = y - mean_y;
        sum_xx += centered_x * centered_x;
        sum_xy += centered_x * centered_y;
        sum_yy += centered_y * centered_y;
    }
    if sum_xx <= f64::EPSILON {
        return (0.0, 0.0);
    }
    let slope = sum_xy / sum_xx;
    let r_squared = if sum_yy <= f64::EPSILON {
        0.0
    } else {
        (sum_xy * sum_xy / (sum_xx * sum_yy)).clamp(0.0, 1.0)
    };
    (slope, r_squared)
}

fn update_leak_state(history: &mut MemoryHistory, suspicious: bool) -> bool {
    if suspicious {
        history.suspicious_windows = history.suspicious_windows.saturating_add(1);
        history.clear_windows = 0;
        if history.suspicious_windows >= LEAK_CONFIRMATION_WINDOWS {
            history.alert_active = true;
        }
    } else {
        history.suspicious_windows = 0;
        if history.alert_active {
            history.clear_windows = history.clear_windows.saturating_add(1);
            if history.clear_windows >= LEAK_CLEAR_WINDOWS {
                history.alert_active = false;
                history.clear_windows = 0;
            }
        }
    }
    history.alert_active
}

fn sort_terminals(terminals: &mut [Terminal], sort: SortBy, descending: bool) {
    terminals.sort_by(|a, b| {
        let order = match sort {
            SortBy::Cpu => a.cpu.total_cmp(&b.cpu),
            SortBy::Memory => a.rss_kib.cmp(&b.rss_kib),
            SortBy::Trend => a.memory_growth_kib.cmp(&b.memory_growth_kib),
            SortBy::Processes => a.processes.len().cmp(&b.processes.len()),
            SortBy::Age => a.age_seconds.cmp(&b.age_seconds),
            SortBy::Activity => a.activity.to_lowercase().cmp(&b.activity.to_lowercase()),
            SortBy::Tab => a
                .label
                .to_lowercase()
                .cmp(&b.label.to_lowercase())
                .then_with(|| natural_tty(&a.tty).cmp(&natural_tty(&b.tty))),
        };
        if descending {
            order.reverse()
        } else {
            order
        }
    });
}

fn natural_tty(tty: &str) -> u32 {
    tty.trim_start_matches("ttys").parse().unwrap_or(u32::MAX)
}

fn render(app: &App) -> Layout {
    let (height, width) = terminal_size();
    let lines = match app.view {
        View::Monitor => render_monitor(app, height, width),
        View::Usage => render_usage(app, height, width),
    };
    print!("\x1b[H\x1b[2J{}", lines.0.join("\n"));
    let _ = io::stdout().flush();
    lines.1
}

/// Builds the monitor frame and its click targets. Size is passed in so the
/// layout can be exercised at any terminal geometry without a real terminal.
fn render_monitor(app: &App, height: usize, width: usize) -> (Vec<String>, Layout) {
    let mut lines = Vec::new();
    let shared_cpu = app.ghostty.as_ref().map_or(0.0, |p| p.cpu);
    let shared_ram = app.ghostty.as_ref().map_or(0, |p| p.rss_kib);
    let terminal_cpu: f64 = app.terminals.iter().map(|t| t.cpu).sum();
    let terminal_ram: u64 = app.terminals.iter().map(|t| t.rss_kib).sum();
    let alert_count = app
        .terminals
        .iter()
        .filter(|terminal| terminal.leak_suspected)
        .count();
    // Stay quiet unless something is actually wrong.
    let alert_status = if app.tracker_outdated {
        format!(
            "  {BOLD}{RED}⚠ background tracker is an old build — rerun --install-tracker{RESET}"
        )
    } else if alert_count == 0 {
        String::new()
    } else {
        format!("  {BOLD}{RED}⚠ {alert_count} potential leak alert(s){RESET}")
    };
    // Naming today's figure is what makes the calendar discoverable: "usage"
    // alone never told anyone what was behind it.
    let tracker = app.usage_tracker.as_ref();
    let today_total = tracker.map_or(0, |value| usage_total_for_day(tracker, &value.current_day));
    let calendar_teaser = if today_total > 0 {
        format!(
            "{DIM}·{RESET}  {BOLD}{}{RESET} {DIM}in Ghostty today · press{RESET} u {DIM}for the calendar{RESET}",
            human_duration(today_total)
        )
    } else {
        format!("{DIM}·  press{RESET} u {DIM}for the usage calendar{RESET}")
    };
    lines.push(format!(
        "{BOLD}{GREEN}ghostty-top{RESET}  {calendar_teaser}{alert_status}",
    ));
    lines.push(format!(
        "Ghostty  {YELLOW}{shared_cpu:.1}%{RESET} CPU {DIM}·{RESET} {CYAN}{}{RESET}    {} terminals  {YELLOW}{terminal_cpu:.1}%{RESET} CPU {DIM}·{RESET} {CYAN}{}{RESET}",
        human_bytes(shared_ram),
        app.terminals.len(),
        human_bytes(terminal_ram),
    ));
    lines.push(String::new());

    let mut layout = Layout {
        terminal_start_row: 5,
        ..Layout::default()
    };

    if app.ghostty.is_none() {
        lines.push("Ghostty is not running, or its process is not visible.".into());
    } else if app.terminals.is_empty() {
        lines.push("No Ghostty terminal processes found.".into());
    } else {
        let (header_line, header_zones) = monitor_header(lines.len() + 1, width, app);
        layout.zones.extend(header_zones);
        lines.push(header_line);
        let max_rows = if app.expanded {
            height.saturating_sub(15).max(3)
        } else {
            height.saturating_sub(8).max(3)
        }
        .min(app.terminals.len());
        let shown_start = if app.selected >= max_rows {
            app.selected + 1 - max_rows
        } else {
            0
        };
        let shown_end = (shown_start + max_rows).min(app.terminals.len());
        layout.shown_start = shown_start;
        layout.shown_count = shown_end - shown_start;
        let (tab_width, activity_width) = monitor_flex_widths(width);
        for (index, terminal) in app
            .terminals
            .iter()
            .enumerate()
            .take(shown_end)
            .skip(shown_start)
        {
            let selected = index == app.selected;
            let mut line = String::new();
            let row_style = if selected {
                SELECTED
            } else if terminal.leak_suspected {
                WARNING
            } else {
                ""
            };
            line.push_str(row_style);
            let marker = if selected { "›" } else { " " };
            line.push_str(&format!(
                "{marker}  {:<tab_width$} {YELLOW}{:>6.1}%{RESET}{row_style} {CYAN}{:>9}{RESET}{row_style} {}{row_style} {:>7} {:>11} {}",
                truncate(&terminal.label, tab_width),
                terminal.cpu,
                human_bytes(terminal.rss_kib),
                trend_cell(terminal),
                terminal.processes.len(),
                terminal.elapsed,
                truncate(&terminal.activity, activity_width),
            ));
            if selected || terminal.leak_suspected {
                line.push_str(RESET);
            }
            lines.push(line);
        }
        if shown_end < app.terminals.len() {
            lines.push(format!(
                "{DIM}   … {} more terminal{} below (scroll or use ↓){RESET}",
                app.terminals.len() - shown_end,
                if app.terminals.len() - shown_end == 1 {
                    ""
                } else {
                    "s"
                }
            ));
        }
    }

    if app.expanded {
        if let Some(terminal) = app.terminals.get(app.selected) {
            lines.push(String::new());
            lines.push(format!(
                "{BOLD}Processes in {}{RESET}  {DIM}(click the selected row to collapse){RESET}",
                terminal.tty
            ));
            if terminal.leak_suspected {
                lines.push(format!(
                    "{BOLD}{RED}⚠ Potential memory leak: {} since tracking began ({}/min){RESET}",
                    signed_human_kib(terminal.memory_growth_kib),
                    signed_human_kib(terminal.memory_slope_kib_per_min.round() as i64),
                ));
            }
            lines.push(format!(
                "{DIM}{UNDERLINE}     PID     CPU       RAM   COMMAND{RESET}"
            ));
            let process_rows = height.saturating_sub(lines.len() + 2);
            let command_width = width.saturating_sub(29).max(12);
            for process in terminal.processes.iter().take(process_rows) {
                lines.push(format!(
                    "  {:>6}  {YELLOW}{:>6.1}%{RESET}  {CYAN}{:>8}{RESET}   {}",
                    process.pid,
                    process.cpu,
                    human_bytes(process.rss_kib),
                    truncate(&process.command, command_width),
                ));
            }
        }
    }

    if let Some(error) = &app.last_error {
        lines.push(format!("\x1b[31mSampling error: {error}{RESET}"));
    }
    lines.push(String::new());
    let (hint, hint_zones) = hint_line(
        lines.len() + 1,
        &[
            ("↑ up", Some(ClickAction::SelectPrevious)),
            ("↓ down", Some(ClickAction::SelectNext)),
            ("enter expand", Some(ClickAction::ToggleExpand)),
            ("u usage", Some(ClickAction::ShowUsage)),
            ("q quit", Some(ClickAction::Quit)),
        ],
    );
    layout.zones.extend(hint_zones);
    lines.push(hint);
    fit_targets(&mut layout, width);
    let lines = lines
        .into_iter()
        .map(|line| fit_to_width(&line, width))
        .collect();
    (lines, layout)
}

fn render_usage(app: &App, height: usize, width: usize) -> (Vec<String>, Layout) {
    const WEEKDAYS: [&str; 7] = ["Su", "Mo", "Tu", "We", "Th", "Fr", "Sa"];
    let weeks = visible_weeks(width);
    let mut lines = vec![format!(
        "{BOLD}{GREEN}ghostty-top{RESET}  {BOLD}usage calendar{RESET}  {DIM}time spent with each Ghostty tab focused{RESET}"
    )];
    let mut layout = Layout::default();
    let tracker = app.usage_tracker.as_ref();
    let today = tracker
        .map(|value| value.current_day.clone())
        .or_else(local_date)
        .unwrap_or_else(|| "1970-01-01".to_string());
    let today_number = date_to_day_number(&today).unwrap_or(0);
    let start = today_number - sunday_index(today_number) - (weeks as i64 - 1) * 7;

    let daily = daily_totals(tracker, start, weeks, today_number);
    let range_total = sum_seconds(daily.iter().copied());
    let busiest = daily.iter().copied().max().unwrap_or(0);

    lines.push(format!(
        "Last {weeks} weeks  •  {} total  •  busiest day {}",
        human_duration(range_total),
        human_duration(busiest),
    ));
    if !TRACKS_FOCUS {
        lines.push(format!("{DIM}Not recording: {FOCUS_UNAVAILABLE}.{RESET}"));
    } else if let Some(error) = tracker.and_then(|value| value.last_error.as_ref()) {
        lines.push(format!("{RED}Tracking paused: {error}{RESET}"));
    } else if let Some(away) = tracker.and_then(|value| value.away) {
        lines.push(format!("{DIM}Paused while {}.{RESET}", away.reason()));
    } else {
        lines.push(format!(
            "{DIM}Counting only while Ghostty is frontmost and you are at the machine.{RESET}"
        ));
    }
    lines.push(String::new());
    lines.push(format!("{DIM}{}{RESET}", month_header(start, weeks)));

    let cell = |layout: &mut Layout, week: usize, row: usize, day: String| {
        layout.calendar_cells.push(CalendarCell {
            start_column: CALENDAR_GUTTER + 1 + week * CALENDAR_CELL_WIDTH,
            end_column: CALENDAR_GUTTER + week * CALENDAR_CELL_WIDTH + CALENDAR_CELL_WIDTH,
            row,
            day,
        });
    };

    let caption = if app.usage_scale == UsageScale::Daily {
        let peak = usage_peak(&daily, UsageScale::Daily);
        for (weekday, label) in WEEKDAYS.iter().enumerate() {
            let row = lines.len() + 1;
            let mut line = format!(" {label} ");
            for week in 0..weeks {
                let index = week * 7 + weekday;
                let number = start + index as i64;
                if number > today_number {
                    line.push_str(&" ".repeat(CALENDAR_CELL_WIDTH));
                    continue;
                }
                let day = day_number_to_date(number);
                let style = if app.selected_day.as_deref() == Some(day.as_str()) {
                    CALENDAR_SELECTED
                } else if app.hovered_day.as_deref() == Some(day.as_str()) {
                    CALENDAR_HOVER
                } else {
                    CALENDAR_RAMP[usage_level(daily[index], peak)]
                };
                line.push_str(&format!("{style}{CALENDAR_BLOCK} {RESET}"));
                cell(&mut layout, week, row, day);
            }
            lines.push(line);
        }
        legend_line()
    } else {
        let values = weekly_usage(&daily, app.usage_scale);
        let peak = values.iter().copied().max().unwrap_or(0);
        let rows = height.saturating_sub(16).clamp(4, 12);
        for row_index in 0..rows {
            let row = lines.len() + 1;
            let rows_below = rows - 1 - row_index;
            let gutter = match row_index {
                0 => format!("{:<CALENDAR_GUTTER$}", "max"),
                index if index + 1 == rows => format!("{:>3} ", "0"),
                _ => " ".repeat(CALENDAR_GUTTER),
            };
            let mut line = format!("{DIM}{gutter}{RESET}");
            for (week, value) in values.iter().enumerate() {
                let day = day_number_to_date(start + week as i64 * 7);
                let style = if app.selected_day.as_deref() == Some(day.as_str()) {
                    CALENDAR_SELECTED
                } else if app.hovered_day.as_deref() == Some(day.as_str()) {
                    CALENDAR_HOVER
                } else {
                    CALENDAR_RAMP[3]
                };
                let glyph = bar_glyph(bar_eighths(*value, peak, rows), rows_below);
                line.push_str(&format!("{style}{glyph}{RESET} "));
                cell(&mut layout, week, row, day);
            }
            lines.push(line);
        }
        if app.usage_scale == UsageScale::Cumulative {
            format!(
                "{}{DIM}Running total  ·  top{RESET} {}",
                " ".repeat(CALENDAR_GUTTER),
                human_duration(peak)
            )
        } else {
            format!(
                "{}{DIM}Each column = 1 week  ·  tallest{RESET} {}",
                " ".repeat(CALENDAR_GUTTER),
                human_duration(peak)
            )
        }
    };

    lines.push(String::new());
    lines.push(caption);
    let (scale_line, scale_zones) = scale_selector(lines.len() + 1, app.usage_scale);
    layout.zones.extend(scale_zones);
    lines.push(scale_line);

    let by_week = app.usage_scale != UsageScale::Daily;
    lines.push(String::new());
    if let Some(day) = &app.hovered_day {
        let days = selection_days(day, by_week);
        lines.push(format!(
            "{BOLD}{}{RESET}  •  {}",
            selection_label(day, by_week),
            human_duration(usage_total_for_days(tracker, &days))
        ));
    } else if range_total == 0 {
        lines.push(format!(
            "{DIM}Nothing recorded yet. Time is logged while Ghostty is frontmost; run{RESET} ghostty-top --install-tracker {DIM}to keep it running.{RESET}"
        ));
    } else {
        lines.push(format!(
            "{DIM}Hover a {} for its total; click it for the tab breakdown.{RESET}",
            if by_week { "column" } else { "square" }
        ));
    }

    if let Some(day) = &app.selected_day {
        let days = selection_days(day, by_week);
        let tabs = usage_breakdown(tracker, &days);
        lines.push(String::new());
        lines.push(format!(
            "{BOLD}{} breakdown{RESET}  •  {}",
            selection_label(day, by_week),
            human_duration(usage_total_for_days(tracker, &days))
        ));
        lines.push(format!(
            "{DIM}{UNDERLINE}  TAB                                      TIME      SHARE{RESET}"
        ));
        let total = sum_seconds(tabs.iter().map(|(_, seconds)| *seconds));
        let available = height.saturating_sub(lines.len() + 4);
        if tabs.is_empty() {
            lines.push(format!(
                "  No focused Ghostty usage recorded for this {}.",
                if by_week { "week" } else { "day" }
            ));
        } else {
            for (name, seconds) in tabs.into_iter().take(available) {
                let share = if total == 0 {
                    0.0
                } else {
                    seconds as f64 / total as f64 * 100.0
                };
                lines.push(format!(
                    "  {:<40} {:>8}   {:>5.1}%",
                    truncate(&name, 40),
                    human_duration(seconds),
                    share
                ));
            }
        }
    }

    lines.push(String::new());
    let (hint, hint_zones) = hint_line(
        lines.len() + 1,
        &[
            ("u back to monitor", Some(ClickAction::ShowMonitor)),
            ("q quit", Some(ClickAction::Quit)),
        ],
    );
    layout.zones.extend(hint_zones);
    lines.push(hint);
    fit_targets(&mut layout, width);
    let lines = lines
        .into_iter()
        .map(|line| fit_to_width(&line, width))
        .collect();
    (lines, layout)
}

/// Days a selected column covers: one day in the grid, a whole week in a chart.
fn selection_days(anchor: &str, by_week: bool) -> Vec<String> {
    let Some(number) = date_to_day_number(anchor) else {
        return Vec::new();
    };
    if !by_week {
        return vec![anchor.to_string()];
    }
    (0..7).map(|day| day_number_to_date(number + day)).collect()
}

fn selection_label(anchor: &str, by_week: bool) -> String {
    if !by_week {
        return friendly_date(anchor);
    }
    let Some(number) = date_to_day_number(anchor) else {
        return anchor.to_string();
    };
    let (_, first_month, first_day) = civil_from_day_number(number);
    let (year, last_month, last_day) = civil_from_day_number(number + 6);
    format!(
        "{} {first_day} – {} {last_day}, {year}",
        MONTH_NAMES[first_month as usize - 1],
        MONTH_NAMES[last_month as usize - 1],
    )
}

fn usage_total_for_days(tracker: Option<&UsageTracker>, days: &[String]) -> u64 {
    sum_seconds(days.iter().map(|day| usage_total_for_day(tracker, day)))
}

/// Focused time per tab across the selected days, busiest first. Totals merge
/// on tab name so a week's worth of the same tab reads as one row.
fn usage_breakdown(tracker: Option<&UsageTracker>, days: &[String]) -> Vec<(String, u64)> {
    let mut totals: BTreeMap<String, u64> = BTreeMap::new();
    for day in days {
        let Some(tabs) = tracker.and_then(|value| value.store.days.get(day)) else {
            continue;
        };
        for usage in tabs.values() {
            let total = totals.entry(usage.name.clone()).or_default();
            *total = total.saturating_add(usage.seconds);
        }
    }
    let mut rows: Vec<(String, u64)> = totals.into_iter().collect();
    rows.sort_by(|(left_name, left), (right_name, right)| {
        right.cmp(left).then_with(|| left_name.cmp(right_name))
    });
    rows
}

fn usage_total_for_day(tracker: Option<&UsageTracker>, day: &str) -> u64 {
    tracker
        .and_then(|value| value.store.days.get(day))
        .map(|tabs| sum_seconds(tabs.values().map(|usage| usage.seconds)))
        .unwrap_or(0)
}

fn sum_seconds(values: impl Iterator<Item = u64>) -> u64 {
    values.fold(0, u64::saturating_add)
}

/// Weeks that fit the terminal, newest on the right, capped at a full year.
fn visible_weeks(width: usize) -> usize {
    (width.saturating_sub(CALENDAR_GUTTER + 1) / CALENDAR_CELL_WIDTH)
        .clamp(CALENDAR_MIN_WEEKS, CALENDAR_MAX_WEEKS)
}

/// Day of the week with Sunday as 0, matching the calendar's row order.
fn sunday_index(day_number: i64) -> i64 {
    (day_number + 4).rem_euclid(7)
}

/// Focused seconds per grid cell, in chronological (and therefore index) order.
fn daily_totals(
    tracker: Option<&UsageTracker>,
    start: i64,
    weeks: usize,
    today_number: i64,
) -> Vec<u64> {
    (0..weeks * 7)
        .map(|index| {
            let number = start + index as i64;
            if number > today_number {
                return 0;
            }
            usage_total_for_day(tracker, &day_number_to_date(number))
        })
        .collect()
}

/// One value per week column: that week's total, or the running total across
/// the range. A week is the unit here because a bar has a single height, unlike
/// the daily grid where every day gets its own square.
fn weekly_usage(daily: &[u64], scale: UsageScale) -> Vec<u64> {
    let totals = daily
        .chunks(7)
        .map(|week| sum_seconds(week.iter().copied()));
    match scale {
        UsageScale::Cumulative => totals
            .scan(0, |running: &mut u64, seconds: u64| {
                *running = running.saturating_add(seconds);
                Some(*running)
            })
            .collect(),
        _ => totals.collect(),
    }
}

/// Height of one bar in eighths of a row, the resolution the block glyphs give.
/// Any non-zero total keeps at least one eighth so a quiet week stays visible
/// instead of vanishing into the baseline.
fn bar_eighths(value: u64, peak: u64, rows: usize) -> usize {
    if value == 0 || peak == 0 {
        return 0;
    }
    let full = (rows * 8) as u64;
    let scaled = (value.saturating_mul(full) + peak / 2) / peak;
    (scaled.max(1) as usize).min(rows * 8)
}

/// The slice of a bar that falls in one row, counting up from the baseline.
fn bar_glyph(eighths: usize, rows_below: usize) -> &'static str {
    const LEVELS: [&str; 9] = [" ", "▁", "▂", "▃", "▄", "▅", "▆", "▇", "█"];
    LEVELS[eighths.saturating_sub(rows_below * 8).min(8)]
}

fn usage_peak(values: &[u64], scale: UsageScale) -> u64 {
    values
        .iter()
        .copied()
        .max()
        .unwrap_or(0)
        .max(scale.brightness_floor())
}

/// Buckets a value into the ramp: 0 is "no usage", 1-4 are quarters of the peak.
fn usage_level(value: u64, peak: u64) -> usize {
    if value == 0 || peak == 0 {
        return 0;
    }
    (value.saturating_mul(4).div_ceil(peak) as usize).clamp(1, 4)
}

/// Month names above the week columns they start in, GitHub-calendar style.
fn month_header(start: i64, weeks: usize) -> String {
    let grid_width = CALENDAR_GUTTER + weeks * CALENDAR_CELL_WIDTH;
    let mut header = " ".repeat(CALENDAR_GUTTER);
    let mut previous_month = None;
    for week in 0..weeks {
        let (_, month, day) = civil_from_day_number(start + week as i64 * 7);
        let starts_here = match previous_month {
            // Only label the leading column when its month really begins there,
            // so a partial first month cannot crowd out the next label.
            None => day <= 7,
            Some(previous) => previous != month,
        };
        previous_month = Some(month);
        let column = CALENDAR_GUTTER + week * CALENDAR_CELL_WIDTH;
        let label = MONTH_NAMES[month as usize - 1];
        if starts_here && column >= header.len() && column + label.len() <= grid_width {
            header.push_str(&" ".repeat(column - header.len()));
            header.push_str(label);
        }
    }
    // A short range can span no month boundary at all; name the month on show
    // rather than leaving an empty band above the grid.
    if header.trim().is_empty() {
        let (_, month, _) = civil_from_day_number(start);
        header = format!(
            "{}{}",
            " ".repeat(CALENDAR_GUTTER),
            MONTH_NAMES[month as usize - 1]
        );
    }
    header
}

fn legend_line() -> String {
    let mut line = format!("{}{DIM}Less{RESET} ", " ".repeat(CALENDAR_GUTTER));
    for color in CALENDAR_RAMP {
        line.push_str(&format!("{color}{CALENDAR_BLOCK}{RESET} "));
    }
    line.push_str(&format!("{DIM}More{RESET}"));
    line
}

/// Renders `daily · weekly · cumulative` and the columns each word occupies.
fn scale_selector(row: usize, active: UsageScale) -> (String, Vec<ClickZone>) {
    const SEPARATOR_WIDTH: usize = 3;
    let mut line = " ".repeat(CALENDAR_GUTTER);
    let mut column = CALENDAR_GUTTER + 1;
    let mut zones = Vec::with_capacity(UsageScale::ALL.len());
    for (index, scale) in UsageScale::ALL.iter().enumerate() {
        if index > 0 {
            line.push_str(&format!("{DIM} · {RESET}"));
            column += SEPARATOR_WIDTH;
        }
        let label = scale.label();
        if *scale == active {
            line.push_str(&format!("{BOLD}{}{label}{RESET}", CALENDAR_RAMP[4]));
        } else {
            line.push_str(&format!("{DIM}{label}{RESET}"));
        }
        zones.push(ClickZone {
            start_column: column,
            end_column: column + label.len() - 1,
            row,
            action: ClickAction::SetScale(*scale),
        });
        column += label.len();
    }
    (line, zones)
}

/// The single dim line of chrome at the bottom of a view. Segments naming an
/// action are also click targets, so the hint doubles as the mouse affordance.
fn hint_line(row: usize, segments: &[(&str, Option<ClickAction>)]) -> (String, Vec<ClickZone>) {
    const SEPARATOR: &str = "  ·  ";
    let mut line = String::from(DIM);
    let mut column = 1;
    let mut zones = Vec::new();
    for (index, (label, action)) in segments.iter().enumerate() {
        if index > 0 {
            line.push_str(SEPARATOR);
            column += SEPARATOR.chars().count();
        }
        line.push_str(label);
        if let Some(action) = action {
            zones.push(ClickZone {
                start_column: column,
                end_column: column + label.chars().count() - 1,
                row,
                action: *action,
            });
        }
        column += label.chars().count();
    }
    line.push_str(RESET);
    (line, zones)
}

/// Renders the sortable column headings and the columns each one occupies.
/// Columns other than the two flexible ones, plus the spaces between all seven.
const MONITOR_FIXED_WIDTH: usize = 3 + 7 + 9 + 12 + 7 + 11 + 6;

/// Splits the space left over after the fixed columns, favouring tab names
/// because they are longer and more distinguishing than activity names.
fn monitor_flex_widths(width: usize) -> (usize, usize) {
    let spare = width.saturating_sub(MONITOR_FIXED_WIDTH);
    let tab = (spare * 3 / 5).clamp(10, 36);
    (tab, spare.saturating_sub(tab).max(8))
}

fn monitor_header(row: usize, width: usize, app: &App) -> (String, Vec<ClickZone>) {
    let (tab_width, activity_width) = monitor_flex_widths(width);
    // Headings are aligned like the values beneath them: text left, numbers right.
    let columns: [(&str, usize, SortBy, bool); 7] = [
        ("TAB", tab_width, SortBy::Tab, false),
        ("CPU", 7, SortBy::Cpu, true),
        ("RAM", 9, SortBy::Memory, true),
        ("TREND", 12, SortBy::Trend, true),
        ("PROCS", 7, SortBy::Processes, true),
        ("AGE", 11, SortBy::Age, true),
        ("ACTIVITY", activity_width, SortBy::Activity, false),
    ];
    const INDENT: usize = 3;
    let mut line = " ".repeat(INDENT);
    let mut column = INDENT + 1;
    let mut zones = Vec::with_capacity(columns.len());
    for (index, (label, cell_width, sort, right_aligned)) in columns.iter().enumerate() {
        if index > 0 {
            line.push(' ');
            column += 1;
        }
        line.push_str(&header_cell(label, *cell_width, *sort, *right_aligned, app));
        let last = index + 1 == columns.len();
        zones.push(ClickZone {
            start_column: column,
            // The final column owns the rest of the row so its wide values stay clickable.
            end_column: if last {
                width.max(column + cell_width)
            } else {
                column + cell_width - 1
            },
            row,
            action: ClickAction::Sort(*sort),
        });
        column += cell_width;
    }
    (line, zones)
}

fn human_duration(seconds: u64) -> String {
    let hours = seconds / 3_600;
    let minutes = (seconds % 3_600) / 60;
    if hours > 0 {
        format!("{hours}h {minutes:02}m")
    } else if minutes > 0 {
        format!("{minutes}m")
    } else if seconds > 0 {
        "<1m".to_string()
    } else {
        "0m".to_string()
    }
}

fn friendly_date(day: &str) -> String {
    const WEEKDAYS: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
    let Some(number) = date_to_day_number(day) else {
        return day.to_string();
    };
    let (year, month, date) = civil_from_day_number(number);
    let weekday = WEEKDAYS[(number + 3).rem_euclid(7) as usize];
    format!(
        "{weekday}, {} {date}, {year}",
        MONTH_NAMES[month as usize - 1]
    )
}

fn date_to_day_number(day: &str) -> Option<i64> {
    let mut fields = day.split('-').filter_map(|value| value.parse().ok());
    let (year, month, date) = (fields.next()?, fields.next()?, fields.next()?);
    Some(days_from_civil(year, month, date))
}

fn day_number_to_date(number: i64) -> String {
    let (year, month, day) = civil_from_day_number(number);
    format!("{year:04}-{month:02}-{day:02}")
}

fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let adjusted_year = year - i64::from(month <= 2);
    let era = if adjusted_year >= 0 {
        adjusted_year
    } else {
        adjusted_year - 399
    } / 400;
    let year_of_era = adjusted_year - era * 400;
    let shifted_month = month + if month > 2 { -3 } else { 9 };
    let day_of_year = (153 * shifted_month + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

fn civil_from_day_number(number: i64) -> (i64, i64, i64) {
    let shifted = number + 719_468;
    let era = if shifted >= 0 {
        shifted
    } else {
        shifted - 146_096
    } / 146_097;
    let day_of_era = shifted - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let mut year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = month_prime + if month_prime < 10 { 3 } else { -9 };
    year += i64::from(month <= 2);
    (year, month, day)
}

fn terminal_size() -> (usize, usize) {
    let output = Command::new("stty")
        .args([STTY_FILE, "/dev/tty", "size"])
        .output();
    let Some(output) = output.ok().filter(|value| value.status.success()) else {
        return (24, 100);
    };
    let text = String::from_utf8_lossy(&output.stdout);
    let mut values = text
        .split_whitespace()
        .filter_map(|value| value.parse().ok());
    (values.next().unwrap_or(24), values.next().unwrap_or(100))
}

fn header_cell(
    label: &str,
    width: usize,
    column: SortBy,
    right_aligned: bool,
    app: &App,
) -> String {
    let text = if app.sort == column {
        format!("{label} {}", if app.descending { "↓" } else { "↑" })
    } else {
        label.to_string()
    };
    // Clamped to the column width so the arrow can never widen a heading and
    // push the whole row out of step with the data beneath it.
    let text = truncate(&text, width);
    let padded = if right_aligned {
        format!("{text:>width$}")
    } else {
        format!("{text:<width$}")
    };
    if app.sort == column {
        format!("{ACTIVE_HEADER}{padded}{RESET}")
    } else {
        format!("{DIM}{UNDERLINE}{padded}{RESET}")
    }
}

fn trend_cell(terminal: &Terminal) -> String {
    let text = if terminal.memory_samples < 2 {
        "· sampling".to_string()
    } else if terminal.leak_suspected {
        format!("⚠ {}", compact_signed_kib(terminal.memory_growth_kib))
    } else if terminal.memory_growth_kib > 0 {
        format!("↑ {}", compact_signed_kib(terminal.memory_growth_kib))
    } else if terminal.memory_growth_kib < 0 {
        format!("↓ {}", compact_signed_kib(terminal.memory_growth_kib))
    } else {
        "→ stable".to_string()
    };
    let padded = format!("{:>12}", truncate(&text, 12));
    if terminal.leak_suspected {
        format!("{BOLD}{RED}{padded}{RESET}")
    } else {
        format!("{DIM}{padded}{RESET}")
    }
}

fn compact_signed_kib(kib: i64) -> String {
    let absolute = kib.unsigned_abs() as f64;
    let sign = if kib >= 0 { "+" } else { "-" };
    if absolute >= 1024.0 * 1024.0 {
        format!("{sign}{:.1}GiB", absolute / (1024.0 * 1024.0))
    } else if absolute >= 1024.0 {
        format!("{sign}{:.0}MiB", absolute / 1024.0)
    } else {
        format!("{sign}{absolute:.0}KiB")
    }
}

fn signed_human_kib(kib: i64) -> String {
    let absolute = kib.unsigned_abs();
    let sign = if kib >= 0 { "+" } else { "-" };
    format!("{sign}{}", human_bytes(absolute))
}

fn print_snapshot(app: &App) {
    if let Some(error) = &app.last_error {
        eprintln!("Error: {error}");
        std::process::exit(1);
    }
    let Some(ghostty) = &app.ghostty else {
        println!("Ghostty is not running.");
        return;
    };
    println!(
        "Ghostty shared: {:>6.1}% CPU  {:>8} RAM",
        ghostty.cpu,
        human_bytes(ghostty.rss_kib)
    );
    println!(
        "{:<24} {:>7}  {:>9}  {:>11}  {:>5}  {:>11}  ACTIVITY",
        "TAB", "CPU", "RAM", "TREND", "PROCS", "AGE"
    );
    for terminal in &app.terminals {
        println!(
            "{:<24} {:>6.1}%  {:>9}  {:>11}  {:>5}  {:>11}  {}{}",
            truncate(&terminal.label, 24),
            terminal.cpu,
            human_bytes(terminal.rss_kib),
            if terminal.memory_samples < 2 {
                "sampling".to_string()
            } else {
                compact_signed_kib(terminal.memory_growth_kib)
            },
            terminal.processes.len(),
            terminal.elapsed,
            terminal.activity,
            if terminal.leak_suspected {
                "  POTENTIAL LEAK"
            } else {
                ""
            }
        );
    }
}

fn human_bytes(kib: u64) -> String {
    let bytes = kib as f64 * 1024.0;
    if bytes >= 1024.0 * 1024.0 * 1024.0 {
        format!("{:.1} GiB", bytes / (1024.0 * 1024.0 * 1024.0))
    } else if bytes >= 1024.0 * 1024.0 {
        format!("{:.1} MiB", bytes / (1024.0 * 1024.0))
    } else {
        format!("{:.0} KiB", bytes / 1024.0)
    }
}

/// Cuts a rendered line to the terminal width, counting only visible columns.
/// Colour escapes are copied through rather than counted, so a long line is
/// clipped at the edge instead of wrapping and tearing the frame apart.
fn fit_to_width(line: &str, width: usize) -> String {
    let mut fitted = String::new();
    let mut visible = 0;
    let mut characters = line.chars();
    let mut clipped = false;
    while let Some(character) = characters.next() {
        if character == '\x1b' {
            fitted.push(character);
            for escape in characters.by_ref() {
                fitted.push(escape);
                if escape.is_ascii_alphabetic() {
                    break;
                }
            }
            continue;
        }
        if visible == width {
            clipped = true;
            break;
        }
        fitted.push(character);
        visible += 1;
    }
    if clipped {
        fitted.push_str(RESET);
    }
    fitted
}

/// Drops click targets the frame has no room to draw and clamps the rest to the
/// visible edge, so no target sits where the user cannot reach it.
fn fit_targets(layout: &mut Layout, width: usize) {
    layout.zones.retain(|zone| zone.start_column <= width);
    for zone in &mut layout.zones {
        zone.end_column = zone.end_column.min(width);
    }
    layout
        .calendar_cells
        .retain(|cell| cell.start_column <= width);
    for cell in &mut layout.calendar_cells {
        cell.end_column = cell.end_column.min(width);
    }
}

fn truncate(value: &str, max_chars: usize) -> String {
    let count = value.chars().count();
    if count <= max_chars {
        return value.to_string();
    }
    value
        .chars()
        .take(max_chars.saturating_sub(1))
        .collect::<String>()
        + "…"
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(pid: i32, ppid: i32, tty: &str, cpu: f64, rss: u64, command: &str) -> Process {
        Process {
            pid,
            ppid,
            tty: tty.into(),
            cpu,
            rss_kib: rss,
            elapsed: "01:00".into(),
            age_seconds: 60,
            command: command.into(),
        }
    }

    #[test]
    fn parses_ps_rows_with_spaced_commands() {
        let process =
            parse_process("  42  1 ttys002  3.5  1024  01:20 node app.js --flag").unwrap();
        assert_eq!(process.pid, 42);
        assert_eq!(process.tty, "ttys002");
        assert_eq!(process.command, "node app.js --flag");
        assert_eq!(process.age_seconds, 80);
    }

    #[test]
    fn parses_process_age_formats() {
        assert_eq!(parse_elapsed("05:30"), Some(330));
        assert_eq!(parse_elapsed("02:05:30"), Some(7_530));
        assert_eq!(parse_elapsed("3-02:05:30"), Some(266_730));
    }

    #[test]
    fn flags_sustained_memory_growth() {
        let start = Instant::now();
        let samples = (0..=12)
            .map(|index| {
                (
                    start + Duration::from_secs(index * 20),
                    (100 + index * 8) * 1024,
                )
            })
            .collect();
        let trend = analyze_memory_history(&samples);
        assert!(trend.growth_kib >= 80 * 1024);
        assert!(trend.suspected);
    }

    #[test]
    fn ignores_short_memory_spikes() {
        let start = Instant::now();
        let samples = VecDeque::from([
            (start, 100 * 1024),
            (start + Duration::from_secs(5), 160 * 1024),
        ]);
        assert!(!analyze_memory_history(&samples).suspected);
    }

    #[test]
    fn ignores_large_allocation_that_plateaus() {
        let start = Instant::now();
        let samples = (0..=12)
            .map(|index| {
                let rss = if index < 3 { 100 * 1024 } else { 220 * 1024 };
                (start + Duration::from_secs(index * 20), rss)
            })
            .collect();
        assert!(!analyze_memory_history(&samples).suspected);
    }

    #[test]
    fn leak_alert_requires_confirmation_and_clears_slowly() {
        let mut history = MemoryHistory::default();
        assert!(!update_leak_state(&mut history, true));
        assert!(!update_leak_state(&mut history, true));
        assert!(update_leak_state(&mut history, true));
        for _ in 0..LEAK_CLEAR_WINDOWS - 1 {
            assert!(update_leak_state(&mut history, false));
        }
        assert!(!update_leak_state(&mut history, false));
    }

    #[test]
    fn process_tree_changes_reset_leak_evidence() {
        let start = Instant::now();
        let mut app = App::new(Duration::from_secs(1));
        let mut terminals = vec![Terminal {
            root_pid: 10,
            tty: "ttys001".into(),
            rss_kib: 100 * 1024,
            processes: vec![p(10, 1, "ttys001", 0.0, 100 * 1024, "zsh")],
            ..Terminal::default()
        }];
        app.update_memory_trends(&mut terminals, start);
        let history = app.memory_history.get_mut(&10).unwrap();
        history.alert_active = true;
        history.suspicious_windows = LEAK_CONFIRMATION_WINDOWS;

        terminals[0]
            .processes
            .push(p(11, 10, "ttys001", 0.0, 80 * 1024, "node server.js"));
        terminals[0].rss_kib = 180 * 1024;
        app.update_memory_trends(&mut terminals, start + Duration::from_secs(1));

        let history = &app.memory_history[&10];
        assert_eq!(history.samples.len(), 1);
        assert!(!history.alert_active);
        assert!(!terminals[0].leak_suspected);
    }

    #[test]
    fn saves_and_loads_daily_tab_usage() {
        let directory =
            env::temp_dir().join(format!("ghostty-top-usage-test-{}", std::process::id()));
        let path = directory.join("usage.tsv");
        let focus = FocusedTab {
            tab_id: "tab-123".into(),
            tab_name: "project shell".into(),
            terminal_id: "terminal-456".into(),
            terminal_name: "zsh".into(),
        };
        let mut store = UsageStore::load(Some(path.clone()));
        store.add("2026-08-06", &focus, 25);
        store.save().unwrap();

        let loaded = UsageStore::load(Some(path));
        assert_eq!(loaded.days["2026-08-06"]["tab-123"].seconds, 25);
        assert_eq!(loaded.days["2026-08-06"]["tab-123"].name, "project shell");
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn demo_history_is_varied_removable_and_preserves_real_usage() {
        let directory =
            env::temp_dir().join(format!("ghostty-top-demo-test-{}", std::process::id()));
        let path = directory.join("usage.tsv");
        let mut original = UsageStore::load(Some(path.clone()));
        original
            .days
            .entry("2026-08-01".into())
            .or_default()
            .insert(
                "real-tab".into(),
                TabUsage {
                    name: "Real work".into(),
                    seconds: 321,
                },
            );
        original.dirty = true;
        original.save().unwrap();

        assert_eq!(
            seed_demo_history_at(Some(path.clone()), "2026-08-06").unwrap(),
            40
        );
        let seeded = UsageStore::load(Some(path.clone()));
        assert!(!seeded.days.contains_key("2026-08-06"));
        assert_eq!(seeded.days["2026-08-01"]["real-tab"].seconds, 321);
        assert!(seeded.days.values().any(|tabs| tabs
            .keys()
            .filter(|id| id.starts_with("demo:"))
            .count()
            > 1));

        assert_eq!(clear_demo_history_at(Some(path.clone())).unwrap(), 40);
        let cleared = UsageStore::load(Some(path));
        assert_eq!(cleared.days.len(), 1);
        assert_eq!(cleared.days["2026-08-01"]["real-tab"].seconds, 321);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn groups_descendants_under_ghostty_terminals() {
        let processes = vec![
            p(
                10,
                1,
                "??",
                2.0,
                100,
                "/Applications/Ghostty.app/Contents/MacOS/ghostty",
            ),
            p(
                20,
                10,
                "ttys001",
                0.0,
                10,
                "/usr/bin/login -flp me /bin/zsh",
            ),
            p(21, 20, "ttys001", 0.0, 20, "-/bin/zsh"),
            p(22, 21, "ttys001", 7.5, 30, "node server.js"),
            p(
                30,
                10,
                "ttys002",
                0.0,
                11,
                "/usr/bin/login -flp me /bin/zsh",
            ),
            p(31, 30, "ttys002", 1.0, 21, "python worker.py"),
        ];
        let (_, terminals) = aggregate(&processes);
        assert_eq!(terminals.len(), 2);
        assert_eq!(terminals[0].rss_kib, 60);
        assert_eq!(terminals[0].activity, "node");
        assert_eq!(terminals[1].cpu, 1.0);
    }

    #[test]
    fn decodes_arrow_and_mouse_input() {
        let mut decoder = InputDecoder::default();
        let events = decoder.push(b"\x1b[A\x1b[<0;18;7M");
        assert_eq!(events[0], InputEvent::Key(Key::Up));
        assert_eq!(
            events[1],
            InputEvent::Mouse(MouseEvent {
                button: 0,
                column: 18,
                row: 7,
                pressed: true,
            })
        );
    }

    #[test]
    fn keeps_partial_mouse_input_until_complete() {
        let mut decoder = InputDecoder::default();
        assert!(decoder.push(b"\x1b[<0;12").is_empty());
        assert_eq!(
            decoder.push(b";5M"),
            vec![InputEvent::Mouse(MouseEvent {
                button: 0,
                column: 12,
                row: 5,
                pressed: true,
            })]
        );
    }

    #[test]
    fn clicking_a_row_selects_then_expands_it() {
        let mut app = App::new(Duration::from_secs(1));
        app.terminals = vec![Terminal::default(), Terminal::default()];
        let layout = Layout {
            terminal_start_row: 5,
            shown_count: 2,
            ..Layout::default()
        };
        let click = MouseEvent {
            button: 0,
            column: 8,
            row: 6,
            pressed: true,
        };
        assert!(app.handle_mouse(click, &layout));
        assert_eq!(app.selected, 1);
        assert!(!app.expanded);
        assert!(app.handle_mouse(click, &layout));
        assert!(app.expanded);
    }

    #[test]
    fn calendar_dates_round_trip() {
        for date in ["1970-01-01", "2024-02-29", "2026-08-06", "2030-12-31"] {
            let number = date_to_day_number(date).unwrap();
            assert_eq!(day_number_to_date(number), date);
        }
        assert_eq!(friendly_date("2026-08-06"), "Thu, Aug 6, 2026");
    }

    #[test]
    fn clicking_calendar_day_selects_it() {
        let mut app = App::new(Duration::from_secs(1));
        app.view = View::Usage;
        let layout = Layout {
            calendar_cells: vec![CalendarCell {
                start_column: 5,
                end_column: 9,
                row: 6,
                day: "2026-08-06".into(),
            }],
            ..Layout::default()
        };
        let click = MouseEvent {
            button: 0,
            column: 7,
            row: 6,
            pressed: true,
        };
        assert!(app.handle_mouse(click, &layout));
        assert_eq!(app.hovered_day.as_deref(), Some("2026-08-06"));
        assert_eq!(app.selected_day.as_deref(), Some("2026-08-06"));
    }

    pub(super) fn tracker_with(
        days: BTreeMap<String, BTreeMap<String, TabUsage>>,
        today: &str,
    ) -> UsageTracker {
        let now = Instant::now();
        UsageTracker {
            store: UsageStore {
                path: None,
                days,
                dirty: false,
            },
            current_day: today.to_string(),
            last_focus: None,
            last_seen: None,
            away: None,
            next_poll: now,
            next_date_check: now,
            next_flush: now,
            next_inventory: now,
            next_inventory_heartbeat: now,
            next_resource_history: now,
            last_inventory: Vec::new(),
            pending_focus: None,
            pending_inventory: None,
            last_error: None,
        }
    }

    /// Drops SGR sequences so a rendered line can be measured in visible columns.
    fn strip_ansi(value: &str) -> String {
        let mut plain = String::new();
        let mut characters = value.chars();
        while let Some(character) = characters.next() {
            if character != '\x1b' {
                plain.push(character);
                continue;
            }
            for escape in characters.by_ref() {
                if escape.is_ascii_alphabetic() {
                    break;
                }
            }
        }
        plain
    }

    fn text_at(line: &str, zone: &ClickZone) -> String {
        let plain = strip_ansi(line);
        let end = zone.end_column.min(plain.chars().count());
        plain
            .chars()
            .skip(zone.start_column - 1)
            .take((end + 1).saturating_sub(zone.start_column))
            .collect()
    }

    #[test]
    fn monitor_headings_line_up_with_their_click_targets() {
        let app = App::new(Duration::from_secs(1));
        let (line, zones) = monitor_header(4, 100, &app);
        let labels = ["TAB", "CPU", "RAM", "TREND", "PROCS", "AGE", "ACTIVITY"];
        assert_eq!(zones.len(), labels.len());
        for (zone, label) in zones.iter().zip(labels) {
            // The sorted column carries a direction arrow inside its own width.
            assert!(text_at(&line, zone).trim().starts_with(label));
            assert_eq!(zone.row, 4);
        }
    }

    #[test]
    fn the_sort_arrow_marks_the_active_column_without_widening_it() {
        let mut app = App::new(Duration::from_secs(1));
        let plain = |app: &App| strip_ansi(&monitor_header(4, 100, app).0);
        let unsorted_width = plain(&app).chars().count();

        app.sort = SortBy::Cpu;
        app.descending = true;
        assert!(plain(&app).contains("CPU ↓"));
        app.descending = false;
        assert!(plain(&app).contains("CPU ↑"));
        assert!(!plain(&app).contains("RAM ↑"));
        assert_eq!(plain(&app).chars().count(), unsorted_width);

        // The widest heading plus an arrow must still not shift the layout.
        app.sort = SortBy::Activity;
        assert_eq!(plain(&app).chars().count(), unsorted_width);
        app.sort = SortBy::Tab;
        assert_eq!(plain(&app).chars().count(), unsorted_width);
    }

    #[test]
    fn headings_keep_step_with_the_rows_beneath_them() {
        // The heading row and the data rows derive their widths from the same
        // split, so the columns cannot drift apart as the terminal resizes.
        for width in [80, 100, 140, 220] {
            let (tab, activity) = monitor_flex_widths(width);
            let app = App::new(Duration::from_secs(1));
            let (line, _) = monitor_header(4, width, &app);
            let plain = strip_ansi(&line);
            let expected = MONITOR_FIXED_WIDTH + tab + activity;
            assert_eq!(plain.chars().count(), expected, "width {width}");
            assert!(plain.trim_start().starts_with("TAB"));
        }
    }

    #[test]
    fn a_tab_name_is_used_only_when_one_surface_owns_the_directory() {
        let mut surfaces: HashMap<&str, Vec<&str>> = HashMap::new();
        surfaces.insert("/work/api", vec!["api server"]);
        surfaces.insert("/work/web", vec!["Recruting", "user@host:~/work/web"]);
        let pick = |directory: &str| match surfaces.get(directory).map(Vec::as_slice) {
            Some([only]) if !only.is_empty() => clean_tab_name(only),
            _ => shorten_path(directory),
        };
        assert_eq!(pick("/work/api"), "api server");
        // Two surfaces share this directory, so neither may claim the name.
        assert_eq!(pick("/work/web"), "/work/web");
    }

    #[test]
    fn generated_shell_titles_are_reduced_to_their_path() {
        assert_eq!(clean_tab_name("me@host:~/src/app"), "~/src/app");
        assert_eq!(clean_tab_name("2nd brain"), "2nd brain");
        // A colon in a hand-written title is not a host prefix.
        assert_eq!(clean_tab_name("build: release"), "build: release");
    }

    #[test]
    fn ghostty_is_recognised_by_path_not_by_pgrep_name() {
        assert!(is_ghostty_app(
            "/Applications/Ghostty.app/Contents/MacOS/ghostty"
        ));
        assert!(is_ghostty_app("ghostty"));
        assert!(!is_ghostty_app("/Users/me/.cargo/bin/ghostty-top"));
        assert!(!is_ghostty_app("ghostty-top"));
    }

    #[test]
    fn sleep_and_stalls_are_not_counted_as_focused_time() {
        let start = Instant::now();
        let wall = SystemTime::now();
        let step = Duration::from_secs(5);
        assert_eq!(
            credited_time((start, wall), (start + step, wall + step)),
            Some(step)
        );

        // Both clocks stalled together: a suspended process, not real use.
        let gap = Duration::from_secs(3_600);
        assert_eq!(
            credited_time((start, wall), (start + gap, wall + gap)),
            None
        );
        // Monotonic clock froze through a sleep but the wall clock kept going.
        assert_eq!(
            credited_time((start, wall), (start + step, wall + gap)),
            None
        );
        // Wall clock jumped backwards.
        assert_eq!(
            credited_time((start, wall + gap), (start + step, wall)),
            None
        );
    }

    #[test]
    fn hint_segments_are_clickable_where_they_are_drawn() {
        // A leading multi-byte segment: columns must be counted in characters,
        // not bytes, or every zone after it lands short of its own text.
        let segments = [
            ("↑↓ select", None),
            ("u usage", Some(ClickAction::ShowUsage)),
            ("q quit", Some(ClickAction::Quit)),
        ];
        let (line, zones) = hint_line(20, &segments);
        let clickable: Vec<&str> = segments
            .iter()
            .filter(|(_, action)| action.is_some())
            .map(|(label, _)| *label)
            .collect();
        assert_eq!(zones.len(), clickable.len());
        for (zone, label) in zones.iter().zip(clickable) {
            assert_eq!(text_at(&line, zone), label);
        }

        let mut app = App::new(Duration::from_secs(1));
        let layout = Layout {
            zones,
            ..Layout::default()
        };
        let quit = MouseEvent {
            button: 0,
            column: layout.zones[1].start_column,
            row: 20,
            pressed: true,
        };
        assert!(!app.handle_mouse(quit, &layout));
    }

    #[test]
    fn calendar_rows_start_on_sunday_and_end_with_today() {
        assert_eq!(sunday_index(date_to_day_number("1970-01-01").unwrap()), 4);
        assert_eq!(sunday_index(date_to_day_number("2026-08-02").unwrap()), 0);
        assert_eq!(sunday_index(date_to_day_number("2026-08-06").unwrap()), 4);

        let today = date_to_day_number("2026-08-06").unwrap();
        let weeks = 12_i64;
        let start = today - sunday_index(today) - (weeks - 1) * 7;
        assert_eq!(sunday_index(start), 0);
        // Today sits in the final column, so the grid never shows a stale week.
        assert!((today - start) >= (weeks - 1) * 7 && (today - start) < weeks * 7);
    }

    #[test]
    fn month_labels_sit_above_the_week_they_begin_in() {
        // 2026-01-04 is a Sunday, so February starts exactly four columns in.
        let start = date_to_day_number("2026-01-04").unwrap();
        let header = month_header(start, 12);
        assert!(header.starts_with(&format!("{}Jan", " ".repeat(CALENDAR_GUTTER))));
        let february = CALENDAR_GUTTER + 4 * CALENDAR_CELL_WIDTH;
        assert!(header[february..].starts_with("Feb"));

        // A partial leading month is left unlabelled rather than crowding the next one.
        let mid_month = date_to_day_number("2026-01-25").unwrap();
        let header = month_header(mid_month, 12);
        assert!(header.trim_start().starts_with("Feb"));

        // A range spanning no boundary still names the month it covers.
        let within = date_to_day_number("2026-03-08").unwrap();
        assert_eq!(month_header(within, 3).trim(), "Mar");
    }

    #[test]
    fn shading_buckets_split_the_peak_into_quarters() {
        assert_eq!(usage_level(0, 100), 0);
        assert_eq!(usage_level(1, 100), 1);
        assert_eq!(usage_level(25, 100), 1);
        assert_eq!(usage_level(26, 100), 2);
        assert_eq!(usage_level(75, 100), 3);
        assert_eq!(usage_level(100, 100), 4);
        // A quiet stretch stays dim instead of maxing out the ramp.
        assert_eq!(usage_peak(&[600], UsageScale::Daily), 4 * 3_600);
        assert_eq!(usage_level(600, usage_peak(&[600], UsageScale::Daily)), 1);
    }

    #[test]
    fn charts_total_each_week_and_run_them_up() {
        let daily: Vec<u64> = (1..=21).collect();
        // 1..7, 8..14, 15..21
        assert_eq!(weekly_usage(&daily, UsageScale::Weekly), [28, 77, 126]);
        assert_eq!(weekly_usage(&daily, UsageScale::Cumulative), [28, 105, 231]);
    }

    #[test]
    fn bars_scale_to_the_peak_and_keep_quiet_weeks_visible() {
        let rows = 4;
        let full = rows * 8;
        assert_eq!(bar_eighths(0, 100, rows), 0);
        assert_eq!(bar_eighths(100, 100, rows), full);
        assert_eq!(bar_eighths(50, 100, rows), full / 2);
        // Rounds to nothing on its own, but never disappears entirely.
        assert_eq!(bar_eighths(1, 100_000, rows), 1);

        // A bar fills whole rows from the baseline up, then a partial glyph.
        assert_eq!(bar_glyph(full, rows - 1), "█");
        assert_eq!(bar_glyph(8 + 4, 0), "█");
        assert_eq!(bar_glyph(8 + 4, 1), "▄");
        assert_eq!(bar_glyph(8 + 4, 2), " ");
    }

    #[test]
    fn chart_columns_cover_whole_weeks_and_grid_cells_single_days() {
        let today = "2026-08-06";
        let today_number = date_to_day_number(today).unwrap();
        let mut days = BTreeMap::new();
        for offset in 0..30_i64 {
            let mut tabs = BTreeMap::new();
            tabs.insert(
                "tab".to_string(),
                TabUsage {
                    name: "editor".into(),
                    seconds: 600 + offset as u64 * 60,
                },
            );
            days.insert(day_number_to_date(today_number - offset), tabs);
        }

        let mut app = App::new(Duration::from_secs(1));
        app.view = View::Usage;
        app.usage_tracker = Some(tracker_with(days, today));

        app.usage_scale = UsageScale::Weekly;
        let (_, chart) = render_usage(&app, 40, 100);
        assert!(!chart.calendar_cells.is_empty());
        for cell in &chart.calendar_cells {
            let number = date_to_day_number(&cell.day).unwrap();
            assert_eq!(sunday_index(number), 0, "columns anchor on a week start");
        }

        // Clicking a column selects the week it stands for.
        let column = chart.calendar_cells[0].clone();
        let click = MouseEvent {
            button: 0,
            column: column.start_column,
            row: column.row,
            pressed: true,
        };
        assert!(app.handle_mouse(click, &chart));
        assert_eq!(app.selected_day.as_deref(), Some(column.day.as_str()));

        app.usage_scale = UsageScale::Daily;
        let (_, grid) = render_usage(&app, 40, 100);
        let anchors: Vec<i64> = grid
            .calendar_cells
            .iter()
            .filter_map(|cell| date_to_day_number(&cell.day))
            .map(sunday_index)
            .collect();
        assert!(
            anchors.iter().any(|index| *index != 0),
            "grid cells are days"
        );
    }

    #[test]
    fn a_chart_column_selects_its_whole_week() {
        assert_eq!(selection_days("2026-08-02", false), ["2026-08-02"]);
        assert_eq!(
            selection_days("2026-08-02", true),
            [
                "2026-08-02",
                "2026-08-03",
                "2026-08-04",
                "2026-08-05",
                "2026-08-06",
                "2026-08-07",
                "2026-08-08",
            ]
        );
        assert_eq!(selection_label("2026-08-02", false), "Sun, Aug 2, 2026");
        assert_eq!(selection_label("2026-08-02", true), "Aug 2 – Aug 8, 2026");
    }

    #[test]
    fn calendar_width_adapts_but_never_exceeds_a_year() {
        assert_eq!(visible_weeks(100), 47);
        assert_eq!(visible_weeks(200), CALENDAR_MAX_WEEKS);
        assert_eq!(visible_weeks(10), CALENDAR_MIN_WEEKS);
    }

    #[test]
    fn calendar_keys_do_not_disturb_the_monitor_sort() {
        let mut app = App::new(Duration::from_secs(1));
        app.view = View::Usage;
        assert!(app.handle_key(Key::Char(b'c')));
        assert_eq!(app.usage_scale, UsageScale::Cumulative);
        assert_eq!(app.sort, SortBy::Memory);

        app.view = View::Monitor;
        assert!(app.handle_key(Key::Char(b'c')));
        assert_eq!(app.sort, SortBy::Cpu);
    }

    #[test]
    fn clicking_a_shading_mode_switches_it() {
        let (line, zones) = scale_selector(9, UsageScale::Daily);
        for (zone, scale) in zones.iter().zip(UsageScale::ALL) {
            assert_eq!(text_at(&line, zone), scale.label());
        }

        let mut app = App::new(Duration::from_secs(1));
        app.view = View::Usage;
        let column = zones[2].start_column;
        let layout = Layout {
            zones,
            ..Layout::default()
        };
        let click = MouseEvent {
            button: 0,
            column,
            row: 9,
            pressed: true,
        };
        assert!(app.handle_mouse(click, &layout));
        assert_eq!(app.usage_scale, UsageScale::Cumulative);
    }

    pub(super) fn populated_app(scale: UsageScale) -> App {
        let today = "2026-08-06";
        let today_number = date_to_day_number(today).unwrap();
        let mut days = BTreeMap::new();
        for offset in 0..400_i64 {
            let mut tabs = BTreeMap::new();
            for index in 0..3 {
                tabs.insert(
                    format!("tab-{index}"),
                    TabUsage {
                        name: format!("a rather long tab name number {index}"),
                        seconds: (offset as u64 * 37 + index * 900) % 20_000,
                    },
                );
            }
            days.insert(day_number_to_date(today_number - offset), tabs);
        }
        let mut app = App::new(Duration::from_secs(1));
        app.usage_scale = scale;
        app.usage_tracker = Some(tracker_with(days, today));
        app.ghostty = Some(p(
            1,
            0,
            "??",
            5.0,
            100_000,
            "/Applications/Ghostty.app/Contents/MacOS/ghostty",
        ));
        for index in 0..12 {
            app.terminals.push(Terminal {
                root_pid: 100 + index,
                tty: format!("ttys{index:03}"),
                label: format!("a tab name that is quite long {index}"),
                cpu: index as f64 * 7.5,
                rss_kib: 4096 * (index as u64 + 1),
                processes: vec![p(
                    200 + index,
                    100 + index,
                    "ttys000",
                    1.0,
                    4096,
                    "node server.js --flag",
                )],
                activity: "node".into(),
                elapsed: "01:02:03".into(),
                age_seconds: 3_723,
                memory_growth_kib: 1024,
                memory_slope_kib_per_min: 12.0,
                memory_samples: 20,
                leak_suspected: index % 4 == 0,
            });
        }
        app
    }

    /// Renders every view at every plausible geometry, including degenerate
    /// ones, so a size-dependent slice or subtraction cannot panic in the wild.
    #[test]
    fn every_view_renders_at_any_terminal_size() {
        for scale in UsageScale::ALL {
            let mut full = populated_app(scale);
            let mut empty = App::new(Duration::from_secs(1));
            empty.usage_scale = scale;

            for app in [&mut full, &mut empty] {
                for (selected, hovered) in [
                    (None, None),
                    (
                        Some("2026-08-02".to_string()),
                        Some("2026-07-19".to_string()),
                    ),
                    // A selection outside the drawn range, and a malformed one.
                    (
                        Some("1999-01-01".to_string()),
                        Some("not-a-date".to_string()),
                    ),
                ] {
                    app.selected_day = selected;
                    app.hovered_day = hovered;
                    for expanded in [false, true] {
                        app.expanded = expanded;
                        for height in [0, 1, 2, 3, 5, 8, 12, 20, 24, 40, 60, 200] {
                            for width in [0, 1, 2, 3, 5, 10, 20, 40, 60, 80, 100, 140, 300] {
                                let (monitor, _) = render_monitor(app, height, width);
                                let (usage, _) = render_usage(app, height, width);
                                assert!(!monitor.is_empty());
                                assert!(!usage.is_empty());
                            }
                        }
                    }
                }
            }
        }
    }

    /// A line wider than the terminal wraps and tears the layout apart, so no
    /// frame may exceed the width it was drawn for.
    #[test]
    fn no_rendered_line_is_wider_than_the_terminal() {
        for scale in UsageScale::ALL {
            let mut app = populated_app(scale);
            app.selected_day = Some("2026-08-02".to_string());
            app.hovered_day = Some("2026-08-02".to_string());
            for expanded in [false, true] {
                app.expanded = expanded;
                for width in [40, 60, 80, 100, 120, 160, 200] {
                    for height in [10, 24, 40, 60] {
                        for (name, frame) in [
                            ("monitor", render_monitor(&app, height, width).0),
                            ("usage", render_usage(&app, height, width).0),
                        ] {
                            for line in frame {
                                let visible = strip_ansi(&line).chars().count();
                                assert!(
                                    visible <= width,
                                    "{name} line of {visible} columns at width {width}: {:?}",
                                    strip_ansi(&line)
                                );
                            }
                        }
                    }
                }
            }
        }
    }

    /// Click targets must land on the frame they were built for; a zone off the
    /// end of a line is a target the user can never hit.
    #[test]
    fn every_click_target_lands_on_its_own_frame() {
        for scale in UsageScale::ALL {
            let app = populated_app(scale);
            for width in [60, 80, 100, 140] {
                for (frame, layout) in [
                    render_monitor(&app, 40, width),
                    render_usage(&app, 40, width),
                ] {
                    let targets = layout
                        .zones
                        .iter()
                        .map(|zone| (zone.row, zone.start_column, zone.end_column))
                        .chain(
                            layout
                                .calendar_cells
                                .iter()
                                .map(|cell| (cell.row, cell.start_column, cell.end_column)),
                        );
                    for (row, start, end) in targets {
                        assert!(row >= 1 && row <= frame.len(), "row {row} is off the frame");
                        assert!(start <= end, "empty target at row {row}");
                        // The last monitor column deliberately runs to the edge.
                        assert!(start <= width, "target starts past the terminal edge");
                    }
                }
            }
        }
    }

    /// The history file is plain text on disk and can be edited, truncated, or
    /// corrupted. Nothing in it may crash the tool or the renderer.
    #[test]
    fn malformed_history_is_survivable() {
        let directory = env::temp_dir().join(format!("ghostty-top-robust-{}", std::process::id()));
        let path = directory.join("usage.tsv");
        fs::create_dir_all(&directory).unwrap();
        let rubbish = [
            "",
            "\t\t\t",
            "not-a-date\ttab\t60\tName",
            "2026-08-06\ttab\tnot-a-number\tName",
            "2026-08-06",
            "2026-08-06\ttab",
            "2026-08-06\ttab\t60",
            "2026-08-06\ttab-ok\t60\tGood",
            // Out-of-range and overflowing date fields.
            "9999-99-99\ttab\t60\tName",
            "-4000-01-01\ttab\t60\tName",
            "999999999999999-01-01\ttab\t60\tName",
            "0000-00-00\ttab\t60\tName",
            // Extremes and unusual text.
            &format!("2026-08-05\thuge\t{}\tBig", u64::MAX),
            "2026-08-04\temoji\t60\t🐈‍⬛ 日本語 tab",
            "2026-08-03\tlong\t60\txxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx",
            "2026-08-02\tansi\t60\tname\x1b[31mwith escape",
        ]
        .join("\n");
        fs::write(&path, rubbish).unwrap();

        let store = UsageStore::load(Some(path.clone()));
        assert_eq!(store.days["2026-08-06"]["tab-ok"].seconds, 60);

        let mut app = App::new(Duration::from_secs(1));
        app.usage_tracker = Some(tracker_with(store.days.clone(), "2026-08-06"));
        for scale in UsageScale::ALL {
            app.usage_scale = scale;
            for day in store.days.keys() {
                app.selected_day = Some(day.clone());
                app.hovered_day = Some(day.clone());
                for width in [40, 100, 200] {
                    let (lines, _) = render_usage(&app, 30, width);
                    for line in lines {
                        assert!(strip_ansi(&line).chars().count() <= width);
                    }
                }
            }
        }

        // Rewriting must not lose or mangle the rows that did parse.
        let mut reloaded = UsageStore::load(Some(path.clone()));
        reloaded.dirty = true;
        reloaded.save().unwrap();
        let again = UsageStore::load(Some(path));
        assert_eq!(again.days["2026-08-06"]["tab-ok"].seconds, 60);
        assert_eq!(again.days["2026-08-04"]["emoji"].name, "🐈‍⬛ 日本語 tab");
        fs::remove_dir_all(directory).unwrap();
    }

    /// Runs the real `ps` with this platform's arguments and parses the result,
    /// which is what catches a wrong flag or column name on a new platform.
    #[test]
    fn reads_the_real_process_table() {
        let processes = sample_processes().expect("ps should run");
        assert!(
            processes.len() > 5,
            "expected a populated process table, got {}",
            processes.len()
        );
        let own_pid = std::process::id() as i32;
        let own = processes
            .iter()
            .find(|process| process.pid == own_pid)
            .expect("this test process should appear in the table");
        assert!(own.ppid > 0, "parsed a nonsense parent pid");
        assert!(!own.command.is_empty(), "parsed an empty command");
        assert!(own.rss_kib > 0, "parsed no resident memory");
    }

    #[test]
    fn a_process_without_a_terminal_is_recognised_on_either_platform() {
        assert!(has_tty("ttys001"));
        assert!(has_tty("pts/3"));
        // macOS writes no tty as `??`, Linux as `?`.
        assert!(!has_tty("??"));
        assert!(!has_tty("?"));
        assert!(!has_tty(""));
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn escapes_launch_agent_paths() {
        assert_eq!(
            xml_escape("A & B <tracker> \"path\""),
            "A &amp; B &lt;tracker&gt; &quot;path&quot;"
        );
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn parses_versioned_tab_inventory_records() {
        let record = [
            "true",
            "window-1",
            "Main",
            "tab-1",
            "Project",
            "2",
            "true",
            "1",
            "terminal-1",
            "zsh",
            "/Users/test/project",
            "true",
        ]
        .join("\x1f")
            + "\x1e";
        let inventory = parse_tab_inventory(&record).unwrap();
        assert_eq!(inventory.len(), 1);
        assert_eq!(inventory[0].tab_name, "Project");
        assert_eq!(inventory[0].tab_index, 2);
        assert_eq!(inventory[0].working_directory, "/Users/test/project");
        assert!(inventory[0].app_frontmost);
        assert!(inventory[0].terminal_focused);
    }

    #[test]
    fn process_history_keeps_identity_without_command_arguments() {
        let process = p(
            42,
            7,
            "ttys001",
            12.5,
            4096,
            "/usr/bin/python3 server.py --token secret",
        );
        let row = process_history_row(1_000, "2026-08-06", "terminal", "ttys001", 7, &process);
        assert!(row.contains("\t42\t7\t12.5\t4096\t"));
        assert!(row.ends_with("\tpython3\t/usr/bin/python3"));
        assert!(!row.contains("server.py"));
        assert!(!row.contains("secret"));
    }
}
