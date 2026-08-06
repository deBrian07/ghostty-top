use std::collections::{HashMap, VecDeque};
use std::env;
use std::io::{self, Read, Write};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

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
const HISTORY_WINDOW: Duration = Duration::from_secs(5 * 60);
const MIN_LEAK_DURATION: Duration = Duration::from_secs(30);
const MIN_LEAK_SAMPLES: usize = 6;
const MIN_LEAK_GROWTH_KIB: i64 = 32 * 1024;
const MIN_LEAK_GROWTH_RATIO: f64 = 0.15;

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

#[derive(Clone, Copy, Debug, Default)]
struct Layout {
    header_row: usize,
    terminal_start_row: usize,
    shown_start: usize,
    shown_count: usize,
    footer_row: usize,
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

#[derive(Default)]
struct MemoryHistory {
    samples: VecDeque<(Instant, u64)>,
}

#[derive(Clone, Copy, Debug, Default)]
struct MemoryTrend {
    growth_kib: i64,
    slope_kib_per_min: f64,
    suspected: bool,
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

    fn handle_mouse(&mut self, mouse: MouseEvent, layout: Layout) -> bool {
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
            terminal.leak_suspected = trend.suspected;
        }
    }
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
    let mut interval = Duration::from_secs(1);
    let args: Vec<String> = env::args().skip(1).collect();
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--once" => once = true,
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

    let mut app = App::new(interval);
    app.refresh();

    if once {
        print_snapshot(&app);
        return;
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
                    InputEvent::Mouse(mouse) => app.handle_mouse(mouse, layout),
                };
                if !keep_running {
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
        print!("\x1b[?1049h\x1b[?25l\x1b[?1000h\x1b[?1006h");
        let _ = io::stdout().flush();
        Some(Self { original })
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = Command::new("stty")
            .args(["-f", "/dev/tty", &self.original])
            .status();
        print!("\x1b[?1000l\x1b[?1006l\x1b[?25h\x1b[?1049l{RESET}");
        let _ = io::stdout().flush();
    }
}

fn usage_and_exit(code: i32) -> ! {
    eprintln!("Usage: ghostty-top [--once] [--interval SECONDS]\n\n  --once              print one sample and exit\n  --interval SECONDS  refresh rate from 0.2 to 60 (default: 1)");
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
    let minutes = duration.as_secs_f64() / 60.0;
    let slope = if minutes > 0.0 { growth / minutes } else { 0.0 };
    let ratio = if baseline > 0.0 {
        growth / baseline
    } else {
        0.0
    };
    MemoryTrend {
        growth_kib: growth.round() as i64,
        slope_kib_per_min: slope,
        suspected: samples.len() >= MIN_LEAK_SAMPLES
            && duration >= MIN_LEAK_DURATION
            && growth >= MIN_LEAK_GROWTH_KIB as f64
            && ratio >= MIN_LEAK_GROWTH_RATIO,
    }
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
        "{DIM}Mouse:{RESET} {UNDERLINE}[TTY]{RESET} {UNDERLINE}[CPU]{RESET} {UNDERLINE}[RAM]{RESET} {UNDERLINE}[TREND]{RESET} {UNDERLINE}[PROCS]{RESET} {UNDERLINE}[AGE]{RESET} {UNDERLINE}[ACTIVITY]{RESET} {UNDERLINE}[EXPAND]{RESET} {UNDERLINE}[QUIT]{RESET}"
    ));
    lines.push(format!(
        "{DIM}Keys: t/c/m/g/p/a/n sort  •  ↑/↓ move  •  enter expand  •  r reverse  •  q quit  •  refresh {:.1}s{RESET}",
        app.interval.as_secs_f64()
    ));
    print!("\x1b[H\x1b[2J{}", lines.join("\n"));
    let _ = io::stdout().flush();
    layout
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
        let samples = VecDeque::from([
            (start, 100 * 1024),
            (start + Duration::from_secs(6), 108 * 1024),
            (start + Duration::from_secs(12), 116 * 1024),
            (start + Duration::from_secs(18), 124 * 1024),
            (start + Duration::from_secs(24), 132 * 1024),
            (start + Duration::from_secs(30), 140 * 1024),
        ]);
        let trend = analyze_memory_history(&samples);
        assert_eq!(trend.growth_kib, 40 * 1024);
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
        assert!(app.handle_mouse(click, layout));
        assert_eq!(app.selected, 1);
        assert!(!app.expanded);
        assert!(app.handle_mouse(click, layout));
        assert!(app.expanded);
    }
}
