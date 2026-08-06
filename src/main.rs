use std::collections::HashMap;
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

#[derive(Clone, Debug)]
struct Process {
    pid: i32,
    ppid: i32,
    tty: String,
    cpu: f64,
    rss_kib: u64,
    elapsed: String,
    command: String,
}

#[derive(Clone, Debug, Default)]
struct Terminal {
    tty: String,
    cpu: f64,
    rss_kib: u64,
    processes: Vec<Process>,
    activity: String,
    elapsed: String,
}

#[derive(Clone, Copy, PartialEq)]
enum SortBy {
    Cpu,
    Memory,
    Tty,
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
        }
    }

    fn refresh(&mut self) {
        match sample_processes() {
            Ok(processes) => {
                let selected_tty = self.terminals.get(self.selected).map(|t| t.tty.clone());
                let (ghostty, mut terminals) = aggregate(&processes);
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

    fn handle_key(&mut self, key: u8) -> bool {
        match key {
            b'q' | 3 => return false,
            b'c' => self.set_sort(SortBy::Cpu),
            b'm' => self.set_sort(SortBy::Memory),
            b't' => self.set_sort(SortBy::Tty),
            b'r' => self.descending = !self.descending,
            b'j' => self.selected = (self.selected + 1).min(self.terminals.len().saturating_sub(1)),
            b'k' => self.selected = self.selected.saturating_sub(1),
            b'e' | b'\n' | b' ' => self.expanded = !self.expanded,
            _ => {}
        }
        true
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
    let mut next_sample = Instant::now() + app.interval;
    let mut dirty = true;
    loop {
        if dirty {
            render(&app);
            dirty = false;
        }
        let mut input = [0_u8; 16];
        if let Ok(count) = stdin.read(&mut input) {
            for key in &input[..count] {
                if !app.handle_key(*key) {
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
        print!("\x1b[?1049h\x1b[?25l");
        let _ = io::stdout().flush();
        Some(Self { original })
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = Command::new("stty")
            .args(["-f", "/dev/tty", &self.original])
            .status();
        print!("\x1b[?25h\x1b[?1049l{RESET}");
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
    let command = fields.collect::<Vec<_>>().join(" ");
    Some(Process {
        pid,
        ppid,
        tty,
        cpu,
        rss_kib,
        elapsed,
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
            tty: root.tty.clone(),
            cpu,
            rss_kib,
            processes: members,
            activity,
            elapsed: root.elapsed.clone(),
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

fn sort_terminals(terminals: &mut [Terminal], sort: SortBy, descending: bool) {
    terminals.sort_by(|a, b| {
        let order = match sort {
            SortBy::Cpu => a.cpu.total_cmp(&b.cpu),
            SortBy::Memory => a.rss_kib.cmp(&b.rss_kib),
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

fn render(app: &App) {
    let mut out = String::from("\x1b[H\x1b[2J");
    let shared_cpu = app.ghostty.as_ref().map_or(0.0, |p| p.cpu);
    let shared_ram = app.ghostty.as_ref().map_or(0, |p| p.rss_kib);
    let terminal_cpu: f64 = app.terminals.iter().map(|t| t.cpu).sum();
    let terminal_ram: u64 = app.terminals.iter().map(|t| t.rss_kib).sum();
    out.push_str(&format!(
        "{BOLD}{GREEN}ghostty-top{RESET}  {DIM}per-terminal process usage on macOS{RESET}\n"
    ));
    out.push_str(&format!(
        "Ghostty shared  CPU {YELLOW}{shared_cpu:>6.1}%{RESET}  RAM {CYAN}{:>8}{RESET}    Terminals ({})  CPU {YELLOW}{terminal_cpu:>6.1}%{RESET}  RAM {CYAN}{:>8}{RESET}\n\n",
        human_bytes(shared_ram), app.terminals.len(), human_bytes(terminal_ram)
    ));

    if app.ghostty.is_none() {
        out.push_str("Ghostty is not running, or its process is not visible.\n");
    } else if app.terminals.is_empty() {
        out.push_str("No Ghostty terminal processes found.\n");
    } else {
        out.push_str(&format!(
            "{DIM}   TERMINAL    CPU       RAM   PROCS   AGE         ACTIVITY{RESET}\n"
        ));
        for (index, terminal) in app.terminals.iter().enumerate() {
            let selected = index == app.selected;
            if selected {
                out.push_str(SELECTED);
            }
            let marker = if selected { "›" } else { " " };
            out.push_str(&format!(
                "{marker}  {:<8} {YELLOW}{:>6.1}%{RESET}  {CYAN}{:>8}{RESET}  {:>5}   {:<10}  {}",
                terminal.tty,
                terminal.cpu,
                human_bytes(terminal.rss_kib),
                terminal.processes.len(),
                terminal.elapsed,
                truncate(&terminal.activity, 42),
            ));
            if selected {
                out.push_str(RESET);
            }
            out.push('\n');
        }
    }

    if app.expanded {
        if let Some(terminal) = app.terminals.get(app.selected) {
            out.push_str(&format!("\n{BOLD}Processes in {}{RESET}  {DIM}(RSS is resident memory; shared pages may be counted more than once){RESET}\n", terminal.tty));
            out.push_str(&format!(
                "{DIM}     PID     CPU       RAM   COMMAND{RESET}\n"
            ));
            for process in terminal.processes.iter().take(12) {
                out.push_str(&format!(
                    "  {:>6}  {YELLOW}{:>6.1}%{RESET}  {CYAN}{:>8}{RESET}   {}\n",
                    process.pid,
                    process.cpu,
                    human_bytes(process.rss_kib),
                    truncate(&process.command, 70),
                ));
            }
        }
    }

    if let Some(error) = &app.last_error {
        out.push_str(&format!("\n\x1b[31mSampling error: {error}{RESET}\n"));
    }
    out.push_str(&format!("\n{DIM}j/k select  e/space expand  c CPU  m memory  t terminal  r reverse  q quit  •  refresh {:.1}s{RESET}", app.interval.as_secs_f64()));
    print!("{out}");
    let _ = io::stdout().flush();
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
    println!("TERMINAL     CPU       RAM  PROCS  ACTIVITY");
    for terminal in &app.terminals {
        println!(
            "{:<10} {:>6.1}%  {:>8}  {:>5}  {}",
            terminal.tty,
            terminal.cpu,
            human_bytes(terminal.rss_kib),
            terminal.processes.len(),
            terminal.activity
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
}
