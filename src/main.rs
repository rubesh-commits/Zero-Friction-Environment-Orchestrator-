use clap::{Parser, Subcommand};
use crossterm::{
    cursor::{Hide, MoveTo, Show},
    event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
    execute, queue,
    style::{Color, Print, ResetColor, SetBackgroundColor, SetForegroundColor},
    terminal::{
        self, disable_raw_mode, enable_raw_mode, Clear, ClearType, EnterAlternateScreen,
        LeaveAlternateScreen,
    },
};
use std::ffi::OsString;
use std::io::{self, BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{self, Child, Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

#[derive(Parser)]
#[command(name = "sand", version, about = "", long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Option<CliCommand>,
}

#[derive(Subcommand)]
enum CliCommand {
    #[command(external_subcommand)]
    External(Vec<OsString>),
}

fn main() {
    enable_ansi();
    let cli = Cli::parse();
    match cli.command {
        None => screen(),
        Some(CliCommand::External(args)) => dispatch(args),
    }
}

const TITLE: [&str; 12] = [
    " ##########      ###      ##        #  #########  ",
    "##              #   #     ###       #  ##      ## ",
    "##             #     #    ####      #  ##       ##",
    "##             #     #    ## ##     #  ##       ##",
    " #########     #######    ##  ##    #  ##       ##",
    "          #    #     #    ##   ##   #  ##       ##",
    "          #    #     #    ##    ##  #  ##       ##",
    "          #    #     #    ##     ## #  ##       ##",
    "          #    #     #    ##      ###  ##       ##",
    "##        #    #     #    ##       ##  ##      ## ",
    " ##########    #     #    ##        #  #########  ",
    "                                                  ",
];
const TITLE_WIDTH: usize = 50;
const INNER_WIDTH: usize = 64;
const GAP: usize = 3;
const MIN_BOX_LINES: usize = 2;
const PLACEHOLDER: &str = "paste the repo or paste the local directory";
const INTRO_SECONDS: f64 = 3.5;

const PURE_BLACK: Color = Color::Rgb { r: 0, g: 0, b: 0 };
const LIGHT_GREY: Color = Color::Rgb {
    r: 192,
    g: 192,
    b: 192,
};
const BORDER_GREY: Color = Color::Rgb {
    r: 96,
    g: 96,
    b: 96,
};
const INK: Color = Color::Rgb { r: 0, g: 0, b: 0 };
const WHITE: Color = Color::Rgb {
    r: 255,
    g: 255,
    b: 255,
};
const SAND_YELLOW: Color = Color::Rgb {
    r: 255,
    g: 200,
    b: 60,
};

fn screen() {
    let mut out = io::stdout();
    if enable_raw_mode().is_err()
        || execute!(
            out,
            EnterAlternateScreen,
            SetBackgroundColor(PURE_BLACK),
            Clear(ClearType::All)
        )
        .is_err()
    {
        let _ = disable_raw_mode();
        render_once();
        return;
    }

    play_intro(&mut out);

    let mut text: Vec<char> = Vec::new();
    let mut cursor = 0usize;
    let mut submit: Option<String> = None;

    loop {
        let _ = draw(&mut out, &text, cursor);
        match event::read() {
            Ok(Event::Key(key)) if key.kind == KeyEventKind::Press => {
                if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL)
                {
                    break;
                }
                match key.code {
                    KeyCode::Char(ch) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                        text.insert(cursor, ch);
                        cursor += 1;
                    }
                    KeyCode::Backspace => {
                        if cursor > 0 {
                            cursor -= 1;
                            text.remove(cursor);
                        }
                    }
                    KeyCode::Delete => {
                        if cursor < text.len() {
                            text.remove(cursor);
                        }
                    }
                    KeyCode::Left => cursor = cursor.saturating_sub(1),
                    KeyCode::Right => cursor = (cursor + 1).min(text.len()),
                    KeyCode::Home => cursor = 0,
                    KeyCode::End => cursor = text.len(),
                    KeyCode::Enter => {
                        let value: String = text.iter().collect();
                        if !value.trim().is_empty() {
                            submit = Some(value.trim().to_string());
                            break;
                        }
                    }
                    KeyCode::Esc => break,
                    _ => {}
                }
            }
            Ok(Event::Paste(pasted)) => {
                let clean: Vec<char> = pasted
                    .chars()
                    .filter(|ch| !matches!(ch, '\n' | '\r' | '\t'))
                    .collect();
                let added = clean.len();
                text.splice(cursor..cursor, clean);
                cursor += added;
            }
            Err(_) => break,
            _ => {}
        }
    }

    if let Some(target) = submit {
        let args = vec![
            OsString::from("run"),
            OsString::from(target),
            OsString::from("--yes"),
        ];
        match spawn_backend(args) {
            Ok(child) => run_page(&mut out, child),
            Err(err) => wait_page(&mut out, vec![err.to_string()]),
        }
    }

    let _ = execute!(
        out,
        LeaveAlternateScreen,
        ResetColor,
        SetBackgroundColor(Color::Reset),
        Show
    );
    let _ = disable_raw_mode();
}

fn render_once() {
    let mut out = io::stdout();
    let _ = execute!(out, SetBackgroundColor(PURE_BLACK), Clear(ClearType::All));
    let _ = draw(&mut out, &[], 0);
    let _ = execute!(
        out,
        Show,
        ResetColor,
        SetBackgroundColor(Color::Reset)
    );
}

fn play_intro(out: &mut io::Stdout) {
    let start = Instant::now();
    loop {
        let progress = (start.elapsed().as_secs_f64() / INTRO_SECONDS).min(1.0);
        let _ = draw_sand(out, progress);
        if progress >= 1.0 {
            break;
        }
        thread::sleep(Duration::from_millis(40));
    }
}

fn draw_sand(out: &mut io::Stdout, progress: f64) -> io::Result<()> {
    let (cols, rows) = terminal::size().unwrap_or((80, 24));
    let cols = if cols == 0 { 80 } else { cols as usize };
    let rows = if rows == 0 { 24 } else { rows as usize };
    let cw = cols.saturating_sub(6).min(50).max(10);
    let ch = rows.saturating_sub(6).min(12).max(4);
    let left = cols.saturating_sub(cw) / 2;
    let top = rows.saturating_sub(ch) / 2;
    let bottom = top + ch - 1;
    let inner_w = cw.saturating_sub(2);
    let inner_h = ch.saturating_sub(1);

    queue!(
        out,
        SetBackgroundColor(PURE_BLACK),
        SetForegroundColor(WHITE),
        Clear(ClearType::All)
    )?;

    for y in top..bottom {
        queue!(
            out,
            MoveTo(left as u16, y as u16),
            SetForegroundColor(BORDER_GREY),
            Print("│"),
            MoveTo((left + cw - 1) as u16, y as u16),
            Print("│")
        )?;
    }
    queue!(
        out,
        MoveTo(left as u16, bottom as u16),
        SetForegroundColor(BORDER_GREY),
        Print("└"),
        Print("─".repeat(cw.saturating_sub(2))),
        Print("┘")
    )?;

    let filled = (progress * inner_h as f64) as usize;
    let fraction = progress * inner_h as f64 - filled as f64;
    for row in 0..inner_h {
        let amount = if row < filled {
            inner_w
        } else if row == filled {
            (fraction * inner_w as f64) as usize
        } else {
            0
        };
        if amount > 0 {
            queue!(
                out,
                MoveTo((left + 1) as u16, (bottom - 1 - row) as u16),
                SetForegroundColor(SAND_YELLOW),
                Print("░".repeat(amount))
            )?;
        }
    }

    let surface = bottom.saturating_sub(filled);
    let stream_top = top.saturating_sub(4);
    if surface > stream_top {
        let span = surface - stream_top;
        let tick = (progress * 60.0) as usize;
        let center = (left + cw / 2).min(left + cw.saturating_sub(2));
        for index in 0..6 {
            let y = stream_top + (tick + index * span / 6) % span;
            let x = (center + index % 2).min(left + cw.saturating_sub(2));
            queue!(
                out,
                MoveTo(x as u16, y as u16),
                SetForegroundColor(SAND_YELLOW),
                Print(".")
            )?;
        }
    }

    out.flush()
}

fn spawn_backend(args: Vec<OsString>) -> io::Result<Child> {
    let binary = backend_binary().ok_or_else(|| {
        io::Error::new(io::ErrorKind::NotFound, "backend not found")
    })?;
    Command::new(binary)
        .args(&args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
}

fn run_page(out: &mut io::Stdout, mut child: Child) {
    let (tx, rx) = mpsc::channel::<String>();
    if let Some(pipe) = child.stdout.take() {
        let sender = tx.clone();
        thread::spawn(move || pump(pipe, sender));
    }
    if let Some(pipe) = child.stderr.take() {
        let sender = tx.clone();
        thread::spawn(move || pump(pipe, sender));
    }
    drop(tx);

    let mut lines: Vec<String> = Vec::new();
    let mut finished = false;
    let mut dirty = true;

    loop {
        let mut received = false;
        while let Ok(line) = rx.try_recv() {
            push_line(&mut lines, line);
            received = true;
        }
        if received {
            dirty = true;
        }
        if dirty {
            let _ = draw_page(out, &lines);
            dirty = false;
        }

        if !finished {
            match child.try_wait() {
                Ok(Some(_)) => {
                    while let Ok(line) = rx.try_recv() {
                        push_line(&mut lines, line);
                    }
                    let _ = draw_page(out, &lines);
                    finished = true;
                }
                Ok(None) => {}
                Err(_) => finished = true,
            }
        }

        if event::poll(Duration::from_millis(50)).unwrap_or(false) {
            if let Ok(Event::Key(key)) = event::read() {
                if key.kind == KeyEventKind::Press {
                    if key.code == KeyCode::Esc
                        || (key.code == KeyCode::Char('c')
                            && key.modifiers.contains(KeyModifiers::CONTROL))
                    {
                        let _ = child.kill();
                        let _ = child.wait();
                        break;
                    }
                    if key.code == KeyCode::Enter && finished {
                        break;
                    }
                }
            }
        }
    }
}

fn pump<R: io::Read + Send + 'static>(pipe: R, tx: std::sync::mpsc::Sender<String>) {
    for line in BufReader::new(pipe).lines() {
        match line {
            Ok(line) => {
                if tx.send(line).is_err() {
                    break;
                }
            }
            Err(_) => break,
        }
    }
}

fn push_line(lines: &mut Vec<String>, line: String) {
    lines.push(line);
    if lines.len() > 2000 {
        lines.drain(..lines.len() - 2000);
    }
}

fn wait_page(out: &mut io::Stdout, lines: Vec<String>) {
    let _ = draw_page(out, &lines);
    loop {
        if event::poll(Duration::from_millis(100)).unwrap_or(false) {
            if let Ok(Event::Key(key)) = event::read() {
                if key.kind == KeyEventKind::Press
                    && matches!(key.code, KeyCode::Enter | KeyCode::Esc)
                {
                    break;
                }
            }
        }
    }
}

fn draw_page(out: &mut io::Stdout, lines: &[String]) -> io::Result<()> {
    let (cols, rows) = terminal::size().unwrap_or((80, 24));
    let cols = if cols == 0 { 80 } else { cols as usize };
    let rows = if rows == 0 { 24 } else { rows as usize };
    let title_left = cols.saturating_sub(TITLE_WIDTH) / 2;

    queue!(
        out,
        SetBackgroundColor(PURE_BLACK),
        SetForegroundColor(WHITE),
        Clear(ClearType::All)
    )?;
    for (index, line) in TITLE.iter().enumerate() {
        queue!(
            out,
            MoveTo(0, index as u16),
            SetForegroundColor(SAND_YELLOW),
            Print(" ".repeat(title_left)),
            Print(line)
        )?;
    }

    let output_start = TITLE.len() + 1;
    let visible = rows.saturating_sub(output_start);
    let skip = lines.len().saturating_sub(visible);
    for (offset, line) in lines.iter().skip(skip).enumerate() {
        let row = output_start + offset;
        if row >= rows {
            break;
        }
        queue!(
            out,
            MoveTo(0, row as u16),
            SetForegroundColor(WHITE),
            Print(line)
        )?;
    }
    queue!(out, Hide)?;
    out.flush()
}

fn draw(out: &mut io::Stdout, text: &[char], cursor: usize) -> io::Result<()> {
    let (cols, rows) = terminal::size().unwrap_or((80, 24));
    let cols = if cols == 0 { 80 } else { cols as usize };
    let rows = if rows == 0 { 24 } else { rows as usize };

    let placeholder_chars: Vec<char> = PLACEHOLDER.chars().collect();
    let is_placeholder = text.is_empty();
    let mut lines = if is_placeholder {
        wrap(&placeholder_chars, INNER_WIDTH)
    } else {
        wrap(text, INNER_WIDTH)
    };
    while lines.len() <= cursor / INNER_WIDTH || lines.len() < MIN_BOX_LINES {
        lines.push(String::new());
    }
    let box_width = INNER_WIDTH + 2;
    let box_height = lines.len() + 2;
    let title_left = cols.saturating_sub(TITLE_WIDTH) / 2;
    let box_left = cols.saturating_sub(box_width) / 2;
    let top = rows.saturating_sub(TITLE.len() + GAP + box_height) / 2;

    queue!(
        out,
        SetBackgroundColor(PURE_BLACK),
        SetForegroundColor(WHITE),
        Clear(ClearType::All)
    )?;

    for (index, line) in TITLE.iter().enumerate() {
        queue!(
            out,
            MoveTo(0, (top + index) as u16),
            SetForegroundColor(SAND_YELLOW),
            Print(" ".repeat(title_left)),
            Print(line)
        )?;
    }

    let box_top = format!("┌{}┐", "─".repeat(INNER_WIDTH));
    let box_bottom = format!("└{}┘", "─".repeat(INNER_WIDTH));
    let mut row = top + TITLE.len() + GAP;
    queue!(
        out,
        MoveTo(box_left as u16, row as u16),
        SetBackgroundColor(LIGHT_GREY),
        SetForegroundColor(BORDER_GREY),
        Print(box_top)
    )?;
    row += 1;

    for line in &lines {
        queue!(
            out,
            MoveTo(box_left as u16, row as u16),
            SetBackgroundColor(LIGHT_GREY),
            SetForegroundColor(BORDER_GREY),
            Print("│"),
            SetForegroundColor(INK),
            Print(format!("{line:<INNER_WIDTH$}")),
            SetForegroundColor(BORDER_GREY),
            Print("│")
        )?;
        row += 1;
    }

    queue!(
        out,
        MoveTo(box_left as u16, row as u16),
        SetBackgroundColor(LIGHT_GREY),
        SetForegroundColor(BORDER_GREY),
        Print(box_bottom),
        SetBackgroundColor(PURE_BLACK)
    )?;

    let cursor_line = cursor / INNER_WIDTH;
    let cursor_col = cursor % INNER_WIDTH;
    queue!(
        out,
        MoveTo(
            (box_left + 1 + cursor_col) as u16,
            (top + TITLE.len() + GAP + 1 + cursor_line) as u16
        ),
        Show
    )?;
    out.flush()
}

fn wrap(text: &[char], width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut current = String::new();
    for ch in text {
        if current.chars().count() == width {
            lines.push(std::mem::take(&mut current));
        }
        current.push(*ch);
    }
    lines.push(current);
    lines
}

fn dispatch(args: Vec<OsString>) {
    let Some(binary) = backend_binary() else {
        eprintln!("backend not found");
        process::exit(1);
    };
    match Command::new(binary).args(&args).status() {
        Ok(status) => process::exit(status.code().unwrap_or(1)),
        Err(err) => {
            eprintln!("{err}");
            process::exit(1);
        }
    }
}

fn backend_binary() -> Option<PathBuf> {
    const LINUX: [&str; 3] = [
        "linuxorchestrator",
        "linux-orchestrator/target/release/cli",
        "linux-orchestrator/target/debug/cli",
    ];
    const WINDOWS: [&str; 2] = [
        "winorchestrator.exe",
        "win-orchestrator/target/release/winorchestrator.exe",
    ];
    let names: &[&str] = if cfg!(windows) { &WINDOWS } else { &LINUX };

    for name in names {
        let direct = PathBuf::from(name);
        if direct.is_file() {
            return Some(direct);
        }
        if let Some(found) = find_in_path(name) {
            return Some(found);
        }
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let candidate = root.join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
        if let Ok(cwd) = std::env::current_dir() {
            let candidate = cwd.join(name);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

fn find_in_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

#[cfg(windows)]
fn enable_ansi() {
    extern "system" {
        fn GetStdHandle(handle: u32) -> isize;
        fn GetConsoleMode(handle: isize, mode: *mut u32) -> i32;
        fn SetConsoleMode(handle: isize, mode: u32) -> i32;
    }
    unsafe {
        const STD_OUTPUT_HANDLE: u32 = 0xFFFF_FFF5;
        const ENABLE_VIRTUAL_TERMINAL_PROCESSING: u32 = 0x0004;
        let handle = GetStdHandle(STD_OUTPUT_HANDLE);
        let mut mode = 0u32;
        if GetConsoleMode(handle, &mut mode) != 0 {
            let _ = SetConsoleMode(handle, mode | ENABLE_VIRTUAL_TERMINAL_PROCESSING);
        }
    }
}

#[cfg(not(windows))]
fn enable_ansi() {}
