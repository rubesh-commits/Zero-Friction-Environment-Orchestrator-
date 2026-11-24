use clap::{Parser, Subcommand};
use crossterm::{
    cursor::{MoveTo, Show},
    event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
    execute, queue,
    style::{Color, Print, ResetColor, SetBackgroundColor, SetForegroundColor},
    terminal::{
        self, disable_raw_mode, enable_raw_mode, Clear, ClearType, EnterAlternateScreen,
        LeaveAlternateScreen,
    },
};
use std::ffi::OsString;
use std::io::{self, Write};
use std::path::PathBuf;
use std::process::{self, Command};

#[derive(Parser)]
#[command(name = "envbox", version, about = "", long_about = None)]
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

const TITLE: [&str; 5] = [
    "##### #   # #   # ####  ###  #   #",
    "#     ##  # #   # #   # #   #  # # ",
    "####  # # # #   # ####  #   #   #  ",
    "#     #  ##  # #  #   # #   #  # # ",
    "##### #   #   #   ####  ###  #   #",
];
const TITLE_WIDTH: usize = 35;
const TITLE_SPLIT: usize = 18;
const INNER_WIDTH: usize = 64;
const GAP: usize = 3;
const MIN_BOX_LINES: usize = 2;
const PLACEHOLDER: &str = "paste the repo or paste the local directory";

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
const TITLE_BLUE: Color = Color::Rgb {
    r: 64,
    g: 128,
    b: 255,
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

    let _ = execute!(
        out,
        LeaveAlternateScreen,
        ResetColor,
        SetBackgroundColor(Color::Reset),
        Show
    );
    let _ = disable_raw_mode();

    if let Some(target) = submit {
        dispatch(vec![
            OsString::from("run"),
            OsString::from(target),
            OsString::from("--yes"),
        ]);
    }
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
            SetForegroundColor(WHITE),
            Print(" ".repeat(title_left)),
            Print(&line[..TITLE_SPLIT]),
            SetForegroundColor(TITLE_BLUE),
            Print(&line[TITLE_SPLIT..])
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
