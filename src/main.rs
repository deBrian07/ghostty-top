use std::collections::{BTreeMap, HashMap, VecDeque};
use std::env;
use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
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
const USAGE_POLL_INTERVAL: Duration = Duration::from_secs(5);
const USAGE_FLUSH_INTERVAL: Duration = Duration::from_secs(30);
const USAGE_SAMPLE_TIMEOUT: Duration = Duration::from_secs(1);
const LAUNCH_AGENT_LABEL: &str = "com.debrian07.ghostty-top.tracker";
const INVENTORY_POLL_INTERVAL: Duration = Duration::from_secs(60);
const INVENTORY_HEARTBEAT: Duration = Duration::from_secs(15 * 60);
const RESOURCE_HISTORY_INTERVAL: Duration = Duration::from_secs(5 * 60);
const HISTORY_SCHEMA_VERSION: &str = "1";

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
    header_row: usize,
    terminal_start_row: usize,
    shown_start: usize,
    shown_count: usize,
    footer_row: usize,
    calendar_cells: Vec<CalendarCell>,
}

#[derive(Clone, Debug)]
struct CalendarCell {
    start_column: usize,
    end_column: usize,
    row: usize,
    day: String,
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

#[derive(Clone, Debug, Default)]
struct Terminal {
    root_pid: i32,
    tty: String,
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

#[derive(Clone, Copy, PartialEq)]
enum SortBy {
    Cpu,
    Memory,
    Trend,
    Processes,
    Age,
    Activity,
    Tty,
}

#[derive(Clone, Copy, PartialEq)]
enum View {
    Monitor,
    Usage,
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
    last_observed: Option<Instant>,
    next_poll: Instant,
    next_date_check: Instant,
    next_flush: Instant,
    next_inventory: Instant,
    next_inventory_heartbeat: Instant,
    next_resource_history: Instant,
    last_inventory: Vec<TabObservation>,
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
    usage_tracker: Option<UsageTracker>,
    view: View,
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
            usage_tracker: None,
            view: View::Monitor,
            hovered_day: None,
            selected_day: None,
        }
    }

    fn refresh(&mut self) {
        match sample_processes() {
            Ok(processes) => {
                let selected_tty = self.terminals.get(self.selected).map(|t| t.tty.clone());
                let (ghostty, mut terminals) = aggregate(&processes);
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
        match key {
            Key::Char(b'q' | 3) => return false,
            Key::Char(b'c') => self.set_sort(SortBy::Cpu),
            Key::Char(b'm') => self.set_sort(SortBy::Memory),
            Key::Char(b'g') => self.set_sort(SortBy::Trend),
            Key::Char(b'p') => self.set_sort(SortBy::Processes),
            Key::Char(b'a') => self.set_sort(SortBy::Age),
            Key::Char(b'n') => self.set_sort(SortBy::Activity),
            Key::Char(b't') => self.set_sort(SortBy::Tty),
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
                } else if mouse.row == layout.footer_row {
                    match mouse.column {
                        8..=16 => self.view = View::Monitor,
                        18..=23 => return false,
                        _ => {}
                    }
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
                if mouse.row == layout.header_row {
                    match mouse.column {
                        1..=12 => self.set_sort(SortBy::Tty),
                        13..=20 => self.set_sort(SortBy::Cpu),
                        21..=30 => self.set_sort(SortBy::Memory),
                        31..=43 => self.set_sort(SortBy::Trend),
                        44..=51 => self.set_sort(SortBy::Processes),
                        52..=63 => self.set_sort(SortBy::Age),
                        64.. => self.set_sort(SortBy::Activity),
                        _ => {}
                    }
                } else if mouse.row >= layout.terminal_start_row
                    && mouse.row < layout.terminal_start_row + layout.shown_count
                {
                    let clicked = layout.shown_start + mouse.row - layout.terminal_start_row;
                    if clicked == self.selected {
                        self.expanded = !self.expanded;
                    } else {
                        self.selected = clicked;
                    }
                } else if mouse.row == layout.footer_row {
                    match mouse.column {
                        8..=12 => self.set_sort(SortBy::Tty),
                        14..=18 => self.set_sort(SortBy::Cpu),
                        20..=24 => self.set_sort(SortBy::Memory),
                        26..=32 => self.set_sort(SortBy::Trend),
                        34..=40 => self.set_sort(SortBy::Processes),
                        42..=46 => self.set_sort(SortBy::Age),
                        48..=57 => self.set_sort(SortBy::Activity),
                        59..=66 => self.expanded = !self.expanded,
                        68..=73 => return false,
                        75..=81 => self.view = View::Usage,
                        _ => {}
                    }
                }
            }
            _ => {}
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
            self.descending = sort != SortBy::Tty;
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
            let Ok(seconds) = seconds.parse() else {
                continue;
            };
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
        entry.seconds += seconds;
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
            last_observed: None,
            next_poll: now,
            next_date_check: now + Duration::from_secs(60),
            next_flush: now + USAGE_FLUSH_INTERVAL,
            next_inventory: now,
            next_inventory_heartbeat: now,
            next_resource_history: now,
            last_inventory: Vec::new(),
            last_error: None,
        };
        if let Err(error) = append_tracker_event("start") {
            tracker.last_error = Some(format!("could not save tracker event: {error}"));
        }
        tracker
    }

    fn tick(&mut self, now: Instant) -> bool {
        if now < self.next_poll {
            return false;
        }
        self.next_poll = now + USAGE_POLL_INTERVAL;
        if now >= self.next_date_check {
            if let Some(day) = local_date() {
                self.current_day = day;
            }
            self.next_date_check = now + Duration::from_secs(60);
        }

        let mut changed = false;
        match query_focused_tab() {
            Ok(focus) => {
                if let (Some(previous), Some(previous_time), Some(current)) =
                    (&self.last_focus, self.last_observed, &focus)
                {
                    let elapsed = now.duration_since(previous_time);
                    if previous.tab_id == current.tab_id && elapsed <= USAGE_POLL_INTERVAL * 3 {
                        self.store
                            .add(&self.current_day, current, elapsed.as_secs());
                        changed = true;
                    }
                }
                self.last_focus = focus;
                self.last_observed = Some(now);
                self.last_error = None;
            }
            Err(error) => {
                self.last_focus = None;
                self.last_observed = None;
                self.last_error = Some(error);
                self.next_poll = now + Duration::from_secs(30);
            }
        }
        if now >= self.next_inventory {
            match query_tab_inventory() {
                Ok(inventory) => {
                    if inventory != self.last_inventory || now >= self.next_inventory_heartbeat {
                        if let Err(error) = append_tab_inventory(&self.current_day, &inventory) {
                            self.last_error = Some(format!("could not save tab history: {error}"));
                        }
                        self.last_inventory = inventory;
                        self.next_inventory_heartbeat = now + INVENTORY_HEARTBEAT;
                    }
                }
                Err(error) => self.last_error = Some(error),
            }
            self.next_inventory = now + INVENTORY_POLL_INTERVAL;
        }
        if now >= self.next_flush {
            if let Err(error) = self.store.save() {
                self.last_error = Some(format!("could not save usage: {error}"));
            }
            self.next_flush = now + USAGE_FLUSH_INTERVAL;
        }
        changed
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

fn default_data_dir() -> Option<PathBuf> {
    env::var_os("HOME").map(|home| {
        PathBuf::from(home)
            .join("Library")
            .join("Application Support")
            .join("ghostty-top")
    })
}

fn launch_agent_path() -> Option<PathBuf> {
    env::var_os("HOME").map(|home| {
        PathBuf::from(home)
            .join("Library")
            .join("LaunchAgents")
            .join(format!("{LAUNCH_AGENT_LABEL}.plist"))
    })
}

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

fn launchd_domain() -> String {
    format!("gui/{}", unsafe { getuid() })
}

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

fn remove_if_present(path: &PathBuf) -> Result<(), String> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("could not remove {}: {error}", path.display())),
    }
}

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
    let output = run_osascript(SCRIPT)?;
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
    let output = run_osascript(SCRIPT)?;
    parse_tab_inventory(&output)
}

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

fn run_osascript(script: &str) -> Result<String, String> {
    let mut child = Command::new("osascript")
        .args(["-e", script])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| format!("could not start Ghostty tracking: {error}"))?;
    let deadline = Instant::now() + USAGE_SAMPLE_TIMEOUT;
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

fn ghostty_is_running() -> bool {
    Command::new("pgrep")
        .args(["-x", "ghostty"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
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
            "-h" | "--help" => usage_and_exit(0),
            _ => usage_and_exit(2),
        }
        index += 1;
    }

    if !cfg!(target_os = "macos") {
        eprintln!("ghostty-top currently supports macOS only.");
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
        print_snapshot(&app);
        return;
    }

    app.enable_usage_tracking();

    if track_only {
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
    fn getuid() -> u32;
}

struct TerminalGuard {
    original: String,
}

impl TerminalGuard {
    fn enter() -> Option<Self> {
        let original = String::from_utf8(
            Command::new("stty")
                .args(["-f", "/dev/tty", "-g"])
                .output()
                .ok()?
                .stdout,
        )
        .ok()?
        .trim()
        .to_string();
        let ok = Command::new("stty")
            .args([
                "-f", "/dev/tty", "-echo", "-icanon", "-isig", "min", "0", "time", "0",
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
            .args(["-f", "/dev/tty", &self.original])
            .status();
        print!("\x1b[?1003l\x1b[?1000l\x1b[?1006l\x1b[?25h\x1b[?1049l{RESET}");
        let _ = io::stdout().flush();
    }
}

fn usage_and_exit(code: i32) -> ! {
    eprintln!("Usage: ghostty-top [OPTIONS]\n\n  --once                print one process sample and exit\n  --track               track focused-tab usage without the TUI\n  --install-tracker     install and start tracking automatically at login\n  --uninstall-tracker   remove the login service but preserve usage history\n  --tracker-status      show the login tracker's launchd status\n  --seed-demo-history   add removable calendar test data for prior days\n  --clear-demo-history  remove demo data without touching real history\n  --interval SECONDS    process refresh rate from 0.2 to 60 (default: 1)");
    std::process::exit(code);
}

fn sample_processes() -> Result<Vec<Process>, String> {
    let output = Command::new("ps")
        .args(["-axo", "pid=,ppid=,tty=,%cpu=,rss=,etime=,command="])
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
        .find(|p| p.command.ends_with("/ghostty") || p.command == "ghostty")
        .cloned();
    let Some(ref app) = ghostty else {
        return (None, Vec::new());
    };

    let roots: Vec<&Process> = processes
        .iter()
        .filter(|p| p.ppid == app.pid && p.tty != "??" && is_login_process(&p.command))
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
        members.sort_by(|a, b| b.rss_kib.cmp(&a.rss_kib));
        let activity = members
            .iter()
            .filter(|p| !is_login_process(&p.command) && !is_shell(&p.command))
            .max_by(|a, b| a.cpu.total_cmp(&b.cpu).then(a.rss_kib.cmp(&b.rss_kib)))
            .map(|p| command_name(&p.command))
            .unwrap_or_else(|| "shell".to_string());
        terminals.push(Terminal {
            root_pid: root.pid,
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
            SortBy::Tty => natural_tty(&a.tty).cmp(&natural_tty(&b.tty)),
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
    match app.view {
        View::Monitor => render_monitor(app),
        View::Usage => render_usage(app),
    }
}

fn render_monitor(app: &App) -> Layout {
    let (height, width) = terminal_size();
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
    let alert_status = if alert_count == 0 {
        format!("{DIM}no leak alerts{RESET}")
    } else {
        format!("{BOLD}{RED}⚠ {alert_count} potential leak alert(s){RESET}")
    };
    lines.push(format!(
        "{BOLD}{GREEN}ghostty-top{RESET}  {DIM}per-terminal process usage{RESET}  •  {alert_status}",
    ));
    lines.push(format!(
        "Ghostty shared  CPU {YELLOW}{shared_cpu:>6.1}%{RESET}  RAM {CYAN}{:>8}{RESET}    Terminals ({})  CPU {YELLOW}{terminal_cpu:>6.1}%{RESET}  RAM {CYAN}{:>8}{RESET}    {DIM}sort: {} {}{RESET}",
        human_bytes(shared_ram), app.terminals.len(), human_bytes(terminal_ram),
        sort_name(app.sort),
        if app.descending { "↓" } else { "↑" }
    ));
    lines.push(String::new());

    let mut layout = Layout {
        header_row: 4,
        terminal_start_row: 5,
        ..Layout::default()
    };

    if app.ghostty.is_none() {
        lines.push("Ghostty is not running, or its process is not visible.".into());
    } else if app.terminals.is_empty() {
        lines.push("No Ghostty terminal processes found.".into());
    } else {
        lines.push(format!(
            "   {} {} {} {} {} {} {}",
            header_cell("TERMINAL", 8, SortBy::Tty, app),
            header_cell("CPU", 7, SortBy::Cpu, app),
            header_cell("RAM", 9, SortBy::Memory, app),
            header_cell("TREND", 12, SortBy::Trend, app),
            header_cell("PROCS", 7, SortBy::Processes, app),
            header_cell("AGE", 11, SortBy::Age, app),
            header_cell("ACTIVITY", 8, SortBy::Activity, app),
        ));
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
        let activity_width = width.saturating_sub(64).max(10);
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
                "{marker}  {:<8} {YELLOW}{:>6.1}%{RESET}{row_style} {CYAN}{:>9}{RESET}{row_style} {}{row_style} {:>7} {:>11} {}",
                terminal.tty,
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
    layout.footer_row = lines.len() + 1;
    lines.push(format!(
        "{DIM}Mouse:{RESET} {UNDERLINE}[TTY]{RESET} {UNDERLINE}[CPU]{RESET} {UNDERLINE}[RAM]{RESET} {UNDERLINE}[TREND]{RESET} {UNDERLINE}[PROCS]{RESET} {UNDERLINE}[AGE]{RESET} {UNDERLINE}[ACTIVITY]{RESET} {UNDERLINE}[EXPAND]{RESET} {UNDERLINE}[QUIT]{RESET} {UNDERLINE}[USAGE]{RESET}"
    ));
    lines.push(format!(
        "{DIM}Keys: t/c/m/g/p/a/n sort  •  ↑/↓ move  •  enter expand  •  r reverse  •  u usage  •  q quit  •  refresh {:.1}s{RESET}",
        app.interval.as_secs_f64()
    ));
    print!("\x1b[H\x1b[2J{}", lines.join("\n"));
    let _ = io::stdout().flush();
    layout
}

fn render_usage(app: &App) -> Layout {
    const WEEKDAYS: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
    const WEEKS: i64 = 8;
    let (height, _) = terminal_size();
    let mut lines = vec![format!(
        "{BOLD}{GREEN}ghostty-top{RESET}  {BOLD}focused tab usage{RESET}"
    )];
    let mut layout = Layout::default();
    let tracker = app.usage_tracker.as_ref();
    let today = tracker
        .map(|value| value.current_day.clone())
        .or_else(local_date)
        .unwrap_or_else(|| "1970-01-01".to_string());
    let today_number = date_to_day_number(&today).unwrap_or(0);
    let current_weekday = (today_number + 3).rem_euclid(7);
    let start = today_number - current_weekday - (WEEKS - 1) * 7;
    let range_total: u64 = tracker
        .map(|value| {
            value
                .store
                .days
                .iter()
                .filter_map(|(day, tabs)| {
                    let number = date_to_day_number(day)?;
                    (number >= start && number <= today_number)
                        .then(|| tabs.values().map(|usage| usage.seconds).sum::<u64>())
                })
                .sum()
        })
        .unwrap_or(0);
    lines.push(format!(
        "Last 8 weeks  •  {} total  •  brighter dots mean more focused time",
        human_duration(range_total)
    ));
    if let Some(error) = tracker.and_then(|value| value.last_error.as_ref()) {
        lines.push(format!("{RED}Tracking paused: {error}{RESET}"));
    } else {
        lines.push(format!(
            "{DIM}Tracking only while Ghostty is frontmost; samples every 5 seconds.{RESET}"
        ));
    }
    lines.push(String::new());

    let mut week_header = String::from("    ");
    for week in 0..WEEKS {
        let (year, month, day) = civil_from_day_number(start + week * 7);
        let _ = year;
        week_header.push_str(&format!("{month:02}/{day:02} "));
    }
    lines.push(format!("{DIM}{week_header}{RESET}"));

    for (weekday, label) in WEEKDAYS.iter().enumerate() {
        let row = lines.len() + 1;
        let mut line = format!("{label} ");
        for week in 0..WEEKS {
            let number = start + week * 7 + weekday as i64;
            let day = day_number_to_date(number);
            let seconds = tracker
                .and_then(|value| value.store.days.get(&day))
                .map(|tabs| tabs.values().map(|usage| usage.seconds).sum())
                .unwrap_or(0);
            let future = number > today_number;
            line.push_str(&usage_dot(seconds, future));
            if !future {
                layout.calendar_cells.push(CalendarCell {
                    start_column: 5 + week as usize * 6,
                    end_column: 10 + week as usize * 6,
                    row,
                    day,
                });
            }
        }
        lines.push(line);
    }

    lines.push(String::new());
    if let Some(day) = &app.hovered_day {
        let seconds = usage_total_for_day(tracker, day);
        lines.push(format!(
            "{BOLD}{}{RESET}  •  {}",
            friendly_date(day),
            human_duration(seconds)
        ));
    } else {
        lines.push(format!(
            "{DIM}Hover over a dot for its date and total; click it for the tab breakdown.{RESET}"
        ));
    }

    if let Some(day) = &app.selected_day {
        lines.push(String::new());
        lines.push(format!(
            "{BOLD}{} breakdown{RESET}  •  {}",
            friendly_date(day),
            human_duration(usage_total_for_day(tracker, day))
        ));
        lines.push(format!(
            "{DIM}{UNDERLINE}  TAB                                      TIME      SHARE{RESET}"
        ));
        let mut tabs: Vec<(&String, &TabUsage)> = tracker
            .and_then(|value| value.store.days.get(day))
            .map(|values| values.iter().collect())
            .unwrap_or_default();
        tabs.sort_by(|(_, a), (_, b)| b.seconds.cmp(&a.seconds));
        let total = tabs.iter().map(|(_, usage)| usage.seconds).sum::<u64>();
        let available = height.saturating_sub(lines.len() + 4);
        if tabs.is_empty() {
            lines.push("  No focused Ghostty usage recorded for this day.".to_string());
        } else {
            for (_, usage) in tabs.into_iter().take(available) {
                let share = if total == 0 {
                    0.0
                } else {
                    usage.seconds as f64 / total as f64 * 100.0
                };
                lines.push(format!(
                    "  {:<40} {:>8}   {:>5.1}%",
                    truncate(&usage.name, 40),
                    human_duration(usage.seconds),
                    share
                ));
            }
        }
    }

    lines.push(String::new());
    layout.footer_row = lines.len() + 1;
    lines.push(format!(
        "{DIM}Mouse:{RESET} {UNDERLINE}[MONITOR]{RESET} {UNDERLINE}[QUIT]{RESET}"
    ));
    lines.push(format!(
        "{DIM}Keys: u monitor  •  q quit  •  data: ~/Library/Application Support/ghostty-top/usage.tsv{RESET}"
    ));
    print!("\x1b[H\x1b[2J{}", lines.join("\n"));
    let _ = io::stdout().flush();
    layout
}

fn usage_total_for_day(tracker: Option<&UsageTracker>, day: &str) -> u64 {
    tracker
        .and_then(|value| value.store.days.get(day))
        .map(|tabs| tabs.values().map(|usage| usage.seconds).sum())
        .unwrap_or(0)
}

fn usage_dot(seconds: u64, future: bool) -> String {
    if future {
        return "      ".to_string();
    }
    let (color, dot) = match seconds {
        0 => ("\x1b[38;5;238m", "·"),
        1..=899 => ("\x1b[38;5;240m", "●"),
        900..=3_599 => ("\x1b[38;5;75m", "●"),
        3_600..=10_799 => ("\x1b[38;5;81m", "●"),
        10_800..=21_599 => ("\x1b[38;5;159m", "●"),
        _ => ("\x1b[38;5;231m", "●"),
    };
    format!("  {color}{dot}{RESET}   ")
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
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let Some(number) = date_to_day_number(day) else {
        return day.to_string();
    };
    let (year, month, date) = civil_from_day_number(number);
    let weekday = WEEKDAYS[(number + 3).rem_euclid(7) as usize];
    format!("{weekday}, {} {date}, {year}", MONTHS[month as usize - 1])
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
        .args(["-f", "/dev/tty", "size"])
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

fn header_cell(label: &str, width: usize, column: SortBy, app: &App) -> String {
    let text = if app.sort == column {
        format!("{}{}", label, if app.descending { "↓" } else { "↑" })
    } else {
        label.to_string()
    };
    let padded = format!("{text:<width$}");
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

fn sort_name(sort: SortBy) -> &'static str {
    match sort {
        SortBy::Cpu => "CPU",
        SortBy::Memory => "memory",
        SortBy::Trend => "memory trend",
        SortBy::Processes => "processes",
        SortBy::Age => "age",
        SortBy::Activity => "activity",
        SortBy::Tty => "terminal",
    }
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
    println!("TERMINAL     CPU       RAM        TREND  PROCS          AGE  ACTIVITY");
    for terminal in &app.terminals {
        println!(
            "{:<10} {:>6.1}%  {:>8}  {:>11}  {:>5}  {:>11}  {}{}",
            terminal.tty,
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

    #[test]
    fn escapes_launch_agent_paths() {
        assert_eq!(
            xml_escape("A & B <tracker> \"path\""),
            "A &amp; B &lt;tracker&gt; &quot;path&quot;"
        );
    }

    #[test]
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
