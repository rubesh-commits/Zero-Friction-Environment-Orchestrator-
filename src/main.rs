use clap::{Parser, Subcommand};
use crossterm::{
    cursor::{self, Hide, MoveTo, Show},
    event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
    execute, queue,
    style::{Color, Print, ResetColor, SetBackgroundColor, SetForegroundColor},
    terminal::{self, disable_raw_mode, enable_raw_mode, Clear, ClearType},
};
use std::cmp::Reverse;
use std::ffi::OsString;
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{self, Child, ChildStdin, Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant, SystemTime};

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
        None => {
            println!("usage: sand run <repository-url-or-path>");
        }
        Some(CliCommand::External(args)) => {
            let is_run = args
                .first()
                .and_then(|value| value.to_str())
                .map(|value| value == "run")
                .unwrap_or(false);
            if is_run && args.len() > 1 {
                run_ui(args);
            } else {
                dispatch(args);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Theme and layout constants.
// ---------------------------------------------------------------------------

const PURE_BLACK: Color = Color::Rgb { r: 0, g: 0, b: 0 };
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

/// A realistic sand dune. It starts completely off-screen on the left, slides
/// right, and stops once it is fully visible directly under the typed command.
const DUNE_ART: [&str; 3] = [
    "          ⡀⢠⡀           ",
    "        ⢠⣶⣿⣿⣿⣶⡄         ",
    "      ⡀⣶⣿⣿⣿⣿⣿⣿⣶⣿⣄       ",
];
const DUNE_ART_WIDTH: usize = 24;
const DUNE_SPEED: f64 = 18.0;
/// Dune-shaped glyphs used instead of `###` in the total progress bar.
const DUNE_FILL: &str = "⢠⣶⣿⣶⡄";
const TAB_LABELS: [&str; 3] = ["Logs", "Files", "Dependencies"];
const HINT: &str = "←/→ tabs · ↑/↓ move · ⏎ entry/select · space mark · p promote · q quit";
const MAX_FILES: usize = 240;
const MAX_DEPS: usize = 200;
const MAX_LOG_LINES: usize = 2000;
/// Keep at least this many rows under the typed command for the dashboard.
const MIN_DASHBOARD_ROWS: usize = 14;

#[derive(Clone, Debug, PartialEq, Eq)]
struct DependencyEntry {
    label: String,
    promotable: bool,
    runtime: Option<String>,
}

#[derive(Clone, Copy)]
struct Geometry {
    cols: usize,
    rows: usize,
    dune_row: usize,
    tab_row: usize,
    content_top: usize,
    progress_row: usize,
}

impl Geometry {
    /// The dune sits directly below the entry point; tabs come after it.
    fn new(anchor_row: usize) -> Self {
        let (raw_cols, raw_rows) = terminal::size().unwrap_or((80, 24));
        let cols = if raw_cols == 0 { 80 } else { raw_cols as usize };
        let rows = if raw_rows == 0 { 24 } else { raw_rows as usize };
        let dune_row = (anchor_row + 1).min(rows.saturating_sub(DUNE_ART.len() + 2));
        let tab_row = (dune_row + DUNE_ART.len()).min(rows.saturating_sub(2));
        let content_top = (tab_row + 1).min(rows.saturating_sub(2));
        let progress_row = rows.saturating_sub(1);
        Self {
            cols,
            rows,
            dune_row,
            tab_row,
            content_top,
            progress_row,
        }
    }

    fn usable_cols(&self) -> usize {
        self.cols.saturating_sub(1)
    }

    fn content_height(&self) -> usize {
        self.progress_row.saturating_sub(self.content_top)
    }
}

// ---------------------------------------------------------------------------
// Entry point: `sand run <target>`.
// ---------------------------------------------------------------------------

fn run_ui(args: Vec<OsString>) {
    let mut out = io::stdout();

    // Query the cursor before switching to raw mode so the dashboard can be
    // anchored underneath the command line that is already on screen.
    let mut anchor_row = cursor::position()
        .map(|(_, row)| row as usize)
        .unwrap_or_else(|_| {
            terminal::size()
                .map(|(_, rows)| rows.saturating_sub(1) as usize)
                .unwrap_or(23)
        });

    // A short window (or a prompt already near the bottom) leaves almost no
    // room below the entry point. Scroll just enough to give the dashboard a
    // usable height without pushing the typed command off screen.
    let rows = terminal::size()
        .map(|(_, rows)| if rows == 0 { 24 } else { rows as usize })
        .unwrap_or(24);
    let scroll = scroll_needed(anchor_row, rows);
    if scroll > 0 {
        let _ = write!(out, "{}", "\n".repeat(scroll));
        let _ = out.flush();
        anchor_row -= scroll;
    }

    if enable_raw_mode().is_err() {
        let _ = execute!(out, Hide);
        let _ = out.flush();
        dispatch(args);
        return;
    }
    let _ = execute!(out, Hide);

    let target = args
        .get(1)
        .and_then(|value| value.to_str())
        .unwrap_or("")
        .to_string();

    // Keep the sandbox alive until the dashboard decides what to do with it.
    // Linux still asks interactively, Windows honours `--keep`; answering the
    // pipe up front means neither backend ever blocks on stdin.
    let base_args = args;
    let mut toolchains_before = snapshot_dir_names(&app_root().join("toolchains"));

    let (tx, rx) = mpsc::channel::<String>();
    let (mut child, mut child_stdin) = match start_backend(&base_args, None, &tx) {
        Ok(session) => (Some(session.0), Some(session.1)),
        Err(err) => {
            let mut lines = Vec::new();
            push_line(&mut lines, format!("[sand] could not start backend: {err}"));
            wait_for_quit(&mut out, anchor_row, &lines);
            cleanup(&mut out);
            return;
        }
    };

    let mut logs: Vec<String> = Vec::new();
    let mut files: Vec<String> = Vec::new();
    let mut deps: Vec<DependencyEntry> = Vec::new();
    let mut dep_selected: Vec<bool> = Vec::new();
    let mut progress = 0.0;
    let mut active_tab = 0usize;
    let mut file_cursor = 0usize;
    let mut dep_cursor = 0usize;
    let mut selected_entry: Option<String> = None;
    let mut views_dirty = true;
    let mut dirty = true;
    let mut close_note_added = false;
    let mut finished = child.is_none();
    let mut finalized = false;
    let mut promote_requested = false;
    let mut promote_names: Vec<String> = Vec::new();
    let mut promotion_status = String::new();

    // The dune starts completely off-screen on the left and stops once it is
    // fully visible at the right edge. It never loops back.
    let dune_started = Instant::now();
    let mut dune_x: i32 = -(DUNE_ART_WIDTH as i32);

    loop {
        let geo = Geometry::new(anchor_row);
        let mut received = false;
        while let Ok(line) = rx.try_recv() {
            push_line(&mut logs, line);
            received = true;
        }
        if received {
            progress = progress_from(&logs, finished);
            if active_tab != 0 {
                views_dirty = true;
            }
            dirty = true;
        }

        // Both backends are kept alive until the run ends; the final keep /
        // discard decision is made by the Dependencies tab below.
        if !close_note_added
            && logs
                .iter()
                .any(|line| line.contains("Keep & Promote instead of destroying?"))
        {
            close_note_added = true;
            push_line(
                &mut logs,
                "[sand] close policy decided by the Dependencies tab".to_string(),
            );
            views_dirty = true;
            dirty = true;
        }

        if !finished {
            match child.as_mut().map(|child| child.try_wait()) {
                Some(Ok(Some(_))) | Some(Err(_)) | None => {
                    while let Ok(line) = rx.try_recv() {
                        push_line(&mut logs, line);
                    }
                    finished = true;
                    progress = progress_from(&logs, true);
                    views_dirty = true;
                    dirty = true;
                    if promote_requested && !finalized {
                        promotion_status = finalize_sandbox(
                            &logs,
                            &toolchains_before,
                            &selected_promotion_names(&deps, &dep_selected),
                        );
                        finalized = true;
                    } else if !finalized {
                        promotion_status =
                            "Run complete. Press p to promote, or q to discard.".to_string();
                    }
                }
                Some(Ok(None)) => {}
            }
        }

        if views_dirty {
            files = collect_files(&target);
            deps = collect_dependencies(&logs);
            sync_dep_selection(&deps, &mut dep_selected, &mut dep_cursor);
            views_dirty = false;
            dirty = true;
        }

        // One-shot slide from off-screen left to the right side of the row.
        let target_x = geo.cols.saturating_sub(DUNE_ART_WIDTH) as i32;
        let next_x = ((dune_started.elapsed().as_secs_f64() * DUNE_SPEED) as i32
            - DUNE_ART_WIDTH as i32)
            .clamp(-(DUNE_ART_WIDTH as i32), target_x);
        let dune_moved = next_x != dune_x;

        if dirty {
            let _ = draw_dashboard(
                &mut out,
                &geo,
                active_tab,
                &logs,
                &files,
                &deps,
                &dep_selected,
                file_cursor,
                dep_cursor,
                selected_entry.as_deref(),
                progress,
                &promotion_status,
                promote_requested,
                next_x,
            );
            dirty = false;
            dune_x = next_x;
        } else if dune_moved {
            let _ = draw_dune(&mut out, &geo, next_x);
            let _ = out.flush();
            dune_x = next_x;
        }

        if event::poll(Duration::from_millis(40)).unwrap_or(false) {
            match event::read() {
                Ok(Event::Key(key)) if key.kind == KeyEventKind::Press => {
                    let ctrl_c = key.code == KeyCode::Char('c')
                        && key.modifiers.contains(KeyModifiers::CONTROL);
                    match key.code {
                        KeyCode::Char('q') | KeyCode::Esc => {
                            stop_child(&mut child, &mut child_stdin);
                            if !finalized {
                                let _ = finalize_sandbox(&logs, &toolchains_before, &[]);
                            }
                            break;
                        }
                        KeyCode::Char('c') if ctrl_c => {
                            stop_child(&mut child, &mut child_stdin);
                            if !finalized {
                                let _ = finalize_sandbox(&logs, &toolchains_before, &[]);
                            }
                            break;
                        }
                        KeyCode::Left | KeyCode::Char('h') => {
                            active_tab = active_tab.saturating_sub(1);
                            views_dirty = true;
                            dirty = true;
                        }
                        KeyCode::Right | KeyCode::Char('l') | KeyCode::Tab => {
                            active_tab = (active_tab + 1).min(TAB_LABELS.len() - 1);
                            views_dirty = true;
                            dirty = true;
                        }
                        KeyCode::Char('1') | KeyCode::Char('2') | KeyCode::Char('3') => {
                            active_tab = match key.code {
                                KeyCode::Char('1') => 0,
                                KeyCode::Char('2') => 1,
                                _ => 2,
                            };
                            views_dirty = true;
                            dirty = true;
                        }
                        KeyCode::Up => {
                            match active_tab {
                                1 => file_cursor = file_cursor.saturating_sub(1),
                                2 => dep_cursor = dep_cursor.saturating_sub(1),
                                _ => {}
                            }
                            dirty = true;
                        }
                        KeyCode::Down => {
                            match active_tab {
                                1 if !files.is_empty() => {
                                    file_cursor = (file_cursor + 1).min(files.len() - 1);
                                }
                                2 if !deps.is_empty() => {
                                    dep_cursor = (dep_cursor + 1).min(deps.len() - 1);
                                }
                                _ => {}
                            }
                            dirty = true;
                        }
                        KeyCode::Enter => {
                            if active_tab == 1 && !files.is_empty() {
                                let index = file_cursor.min(files.len() - 1);
                                let chosen = files[index].clone();
                                if let Some(entry_index) = entrypoint_index(&files, &chosen) {
                                    selected_entry = Some(chosen.clone());
                                    stop_child(&mut child, &mut child_stdin);
                                    if !finalized {
                                        let _ = finalize_sandbox(&logs, &toolchains_before, &[]);
                                    }
                                    toolchains_before =
                                        snapshot_dir_names(&app_root().join("toolchains"));
                                    match start_backend(&base_args, Some(entry_index), &tx) {
                                        Ok(session) => {
                                            child = Some(session.0);
                                            child_stdin = Some(session.1);
                                        }
                                        Err(_) => {
                                            child = None;
                                            child_stdin = None;
                                        }
                                    }
                                    let restart_error = child.is_none();
                                    logs.clear();
                                    files.clear();
                                    deps.clear();
                                    dep_selected.clear();
                                    progress = 0.0;
                                    file_cursor = 0;
                                    dep_cursor = 0;
                                    finished = child.is_none();
                                    finalized = false;
                                    promote_requested = false;
                                    promote_names.clear();
                                    close_note_added = false;
                                    promotion_status = if restart_error {
                                        format!("Could not restart with entry point {entry_index}: backend unavailable.")
                                    } else {
                                        format!(
                                            "Entry point {entry_index} ({chosen}) selected; restarting sandbox..."
                                        )
                                    };
                                    views_dirty = true;
                                    dirty = true;
                                } else {
                                    promotion_status =
                                        "That file is not a detected entry point.".to_string();
                                    dirty = true;
                                }
                            } else if active_tab == 2 && !deps.is_empty() {
                                if let Some(status) = toggle_dependency(
                                    &deps,
                                    &mut dep_selected,
                                    dep_cursor.min(deps.len() - 1),
                                ) {
                                    promotion_status = status;
                                }
                                dirty = true;
                            }
                        }
                        KeyCode::Char(' ') if active_tab == 2 && !deps.is_empty() => {
                            if let Some(status) = toggle_dependency(
                                &deps,
                                &mut dep_selected,
                                dep_cursor.min(deps.len() - 1),
                            ) {
                                promotion_status = status;
                            }
                            dirty = true;
                        }
                        KeyCode::Char('p') | KeyCode::Char('P') => {
                            promote_names = selected_promotion_names(&deps, &dep_selected);
                            if promote_names.is_empty() {
                                promotion_status =
                                    "No dependencies marked. Move with ↑/↓ and mark with space."
                                        .to_string();
                            } else {
                                promote_requested = true;
                                promotion_status = if finished && !finalized {
                                    finalized = true;
                                    finalize_sandbox(&logs, &toolchains_before, &promote_names)
                                } else if finalized {
                                    "Sandbox already closed; nothing left to promote.".to_string()
                                } else {
                                    format!(
                                        "Promotion queued for {} marked runtime(s).",
                                        promote_names.len()
                                    )
                                };
                            }
                            dirty = true;
                        }
                        _ => {}
                    }
                }
                Ok(Event::Resize(..)) => {
                    dirty = true;
                }
                _ => {}
            }
        }
    }

    cleanup(&mut out);
}

/// How many newlines to emit so at least `MIN_DASHBOARD_ROWS` rows remain
/// below the command, without scrolling the command itself off screen.
fn scroll_needed(anchor_row: usize, rows: usize) -> usize {
    let free_rows = rows.saturating_sub(anchor_row + 1);
    if free_rows < MIN_DASHBOARD_ROWS {
        (MIN_DASHBOARD_ROWS - free_rows).min(anchor_row)
    } else {
        0
    }
}

fn cleanup(out: &mut io::Stdout) {
    let _ = execute!(out, Show, ResetColor, SetBackgroundColor(Color::Reset));
    let _ = disable_raw_mode();
}

fn wait_for_quit(out: &mut io::Stdout, anchor_row: usize, lines: &[String]) {
    let geo = Geometry::new(anchor_row);
    let _ = draw_dashboard(
        out,
        &geo,
        0,
        lines,
        &[],
        &[],
        &[],
        0,
        0,
        None,
        1.0,
        "",
        false,
        0,
    );
    loop {
        if event::poll(Duration::from_millis(100)).unwrap_or(false) {
            match event::read() {
                Ok(Event::Key(key))
                    if key.kind == KeyEventKind::Press
                        && matches!(
                            key.code,
                            KeyCode::Char('q') | KeyCode::Esc | KeyCode::Enter
                        ) =>
                {
                    break;
                }
                _ => {}
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Rendering. Everything is confined to the rows below the entry point.
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
fn draw_dashboard(
    out: &mut io::Stdout,
    geo: &Geometry,
    active_tab: usize,
    logs: &[String],
    files: &[String],
    deps: &[DependencyEntry],
    dep_selected: &[bool],
    file_cursor: usize,
    dep_cursor: usize,
    selected_entry: Option<&str>,
    progress: f64,
    promotion_status: &str,
    promote_requested: bool,
    dune_x: i32,
) -> io::Result<()> {
    queue!(
        out,
        SetBackgroundColor(PURE_BLACK),
        SetForegroundColor(WHITE)
    )?;

    // Pure solid background from the dune row down to the progress bar.
    for row in geo.dune_row..geo.rows {
        queue!(out, MoveTo(0, row as u16), Clear(ClearType::CurrentLine))?;
    }

    draw_dune(out, geo, dune_x)?;
    draw_tabs(out, geo, active_tab)?;
    draw_content(
        out,
        geo,
        active_tab,
        files,
        deps,
        dep_selected,
        file_cursor,
        dep_cursor,
        selected_entry,
        logs,
        promotion_status,
        promote_requested,
    )?;
    draw_progress(out, geo, progress)?;
    queue!(out, Hide)?;
    out.flush()
}

fn draw_tabs(out: &mut io::Stdout, geo: &Geometry, active_tab: usize) -> io::Result<()> {
    let mut x = 0usize;
    for (index, label) in TAB_LABELS.iter().enumerate() {
        if x >= geo.usable_cols() {
            break;
        }
        let active = index == active_tab;
        let text = format!("{}  ", label);
        let text = truncate(&text, geo.usable_cols().saturating_sub(x));
        queue!(
            out,
            MoveTo(x as u16, geo.tab_row as u16),
            SetForegroundColor(if active { SAND_YELLOW } else { WHITE }),
            Print(text)
        )?;
        x += label.len() + 2;
    }

    let hint = truncate(HINT, geo.usable_cols());
    let hint_x = geo.usable_cols().saturating_sub(hint.chars().count());
    if hint_x > x {
        queue!(
            out,
            MoveTo(hint_x as u16, geo.tab_row as u16),
            SetForegroundColor(WHITE),
            Print(hint)
        )?;
    }
    Ok(())
}

/// Visible slice of the dune for a horizontal position that may be negative
/// (off-screen left) or partially past the right edge.
fn dune_visible_range(x: i32, cols: usize) -> (usize, usize, usize) {
    let start = (-x).max(0) as usize;
    let end = ((cols as i32 - x).max(0) as usize).min(DUNE_ART_WIDTH);
    let screen_x = x.max(0) as usize;
    (start, end.min(DUNE_ART_WIDTH), screen_x)
}

/// Paint only the dune rows. Everything else on the screen stays untouched.
fn draw_dune(out: &mut io::Stdout, geo: &Geometry, x: i32) -> io::Result<()> {
    let (start, end, screen_x) = dune_visible_range(x, geo.cols);
    for (offset, art) in DUNE_ART.iter().enumerate() {
        let row = geo.dune_row + offset;
        if row >= geo.rows {
            break;
        }
        let visible: String = art
            .chars()
            .skip(start)
            .take(end.saturating_sub(start))
            .collect();
        queue!(
            out,
            SetBackgroundColor(PURE_BLACK),
            MoveTo(0, row as u16),
            Clear(ClearType::CurrentLine),
            MoveTo(screen_x as u16, row as u16),
            SetForegroundColor(SAND_YELLOW),
            Print(visible)
        )?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn draw_content(
    out: &mut io::Stdout,
    geo: &Geometry,
    active_tab: usize,
    files: &[String],
    deps: &[DependencyEntry],
    dep_selected: &[bool],
    file_cursor: usize,
    dep_cursor: usize,
    selected_entry: Option<&str>,
    logs: &[String],
    promotion_status: &str,
    promote_requested: bool,
) -> io::Result<()> {
    let height = geo.content_height();
    if height == 0 {
        return Ok(());
    }

    let (lines, cursor_row): (Vec<String>, Option<usize>) = match active_tab {
        0 => {
            let mut rows = Vec::new();
            if !promotion_status.is_empty() {
                rows.push(format!("> {}", sanitize(promotion_status)));
            }
            let log_height = height.saturating_sub(rows.len());
            let skip = logs.len().saturating_sub(log_height);
            rows.extend(logs[skip..].iter().map(|line| sanitize(line)));
            (rows, None)
        }
        1 => {
            let mut rows = vec![format!(
                "entrypoint: {} (⏎ on a marked file to change)",
                selected_entry.unwrap_or("auto")
            )];
            if !promotion_status.is_empty() {
                rows.push(format!("> {}", sanitize(promotion_status)));
            }
            let header_rows = rows.len();
            if files.is_empty() {
                rows.push("Waiting for repository files...".to_string());
            } else {
                for (index, file) in files
                    .iter()
                    .take(height.saturating_sub(header_rows))
                    .enumerate()
                {
                    let caret = if index == file_cursor { '>' } else { ' ' };
                    let mark = if is_entrypoint_candidate(file) {
                        'E'
                    } else {
                        ' '
                    };
                    rows.push(format!("{caret} [{mark}] {}", sanitize(file)));
                }
            }
            let cursor = if files.is_empty() {
                None
            } else {
                Some(header_rows + file_cursor)
            };
            (rows, cursor)
        }
        _ => {
            let mut rows = vec![if promote_requested {
                format!("> {}", sanitize(promotion_status))
            } else {
                "> space: mark runtime · p: promote marked".to_string()
            }];
            if deps.is_empty() {
                rows.push("No downloads yet - compatible host runtimes are reused.".to_string());
            } else {
                for (index, dep) in deps.iter().take(height.saturating_sub(1)).enumerate() {
                    let caret = if index == dep_cursor { '>' } else { ' ' };
                    let mark = if !dep.promotable {
                        '-'
                    } else if dep_selected.get(index).copied().unwrap_or(false) {
                        'x'
                    } else {
                        ' '
                    };
                    rows.push(format!("{caret} [{mark}] {}", sanitize(&dep.label)));
                }
            }
            let cursor = if deps.is_empty() {
                None
            } else {
                Some(dep_cursor + 1)
            };
            (rows, cursor)
        }
    };

    for (offset, line) in lines.iter().enumerate().take(height) {
        let row = geo.content_top + offset;
        if row >= geo.progress_row {
            break;
        }
        let color = if cursor_row == Some(offset) {
            SAND_YELLOW
        } else {
            WHITE
        };
        queue!(
            out,
            MoveTo(0, row as u16),
            SetForegroundColor(color),
            Print(format!(
                "{:<width$}",
                truncate(line, geo.usable_cols()),
                width = geo.usable_cols()
            ))
        )?;
    }
    Ok(())
}

fn draw_progress(out: &mut io::Stdout, geo: &Geometry, progress: f64) -> io::Result<()> {
    let label = "total ";
    let width = geo
        .usable_cols()
        .saturating_sub(label.chars().count() + 6)
        .clamp(8, 40);
    let percent = (progress.clamp(0.0, 1.0) * 100.0) as usize;
    let filled = (progress.clamp(0.0, 1.0) * width as f64) as usize;

    queue!(
        out,
        MoveTo(0, geo.progress_row as u16),
        SetForegroundColor(WHITE),
        Print(label)
    )?;
    queue!(
        out,
        MoveTo(label.chars().count() as u16, geo.progress_row as u16),
        SetForegroundColor(SAND_YELLOW),
        Print(repeat_to_width(DUNE_FILL, filled)),
        SetForegroundColor(WHITE),
        Print("░".repeat(width.saturating_sub(filled))),
        Print(format!(" {:>3}%", percent))
    )?;
    Ok(())
}

fn repeat_to_width(pattern: &str, width: usize) -> String {
    let chars: Vec<char> = pattern.chars().collect();
    if chars.is_empty() || width == 0 {
        return String::new();
    }
    (0..width).map(|index| chars[index % chars.len()]).collect()
}

// ---------------------------------------------------------------------------
// Backend plumbing.
// ---------------------------------------------------------------------------

fn spawn_backend(args: Vec<OsString>) -> io::Result<Child> {
    let binary = backend_binary()
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "backend not found"))?;
    Command::new(binary)
        .args(&args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
}

/// Ask both backends to keep the sandbox so the dashboard can decide later.
/// Windows supports `--no-update-path`; Linux ignores unknown flags. An
/// entry-point selection is forwarded with each backend's own flag spelling.
fn backend_run_args(args: &[OsString], entrypoint: Option<usize>) -> Vec<OsString> {
    let mut args = args.to_vec();
    let has_policy = args.iter().skip(2).any(|arg| {
        matches!(
            arg.to_str(),
            Some("--keep" | "--discard" | "--yes" | "-k" | "-d")
        )
    });
    if !has_policy {
        args.push(OsString::from("--keep"));
        args.push(OsString::from("--no-update-path"));
    }
    let has_entrypoint = args.iter().any(|arg| {
        arg.to_str()
            .is_some_and(|value| value.starts_with("--entry=") || value == "--entrypoint")
    });
    if !has_entrypoint {
        if let Some(index) = entrypoint {
            #[cfg(windows)]
            {
                args.push(OsString::from("--entrypoint"));
                args.push(OsString::from(index.to_string()));
            }
            #[cfg(not(windows))]
            args.push(OsString::from(format!("--entry={index}")));
        }
    }
    args
}

/// Spawn a backend and route its output into the dashboard. The stdin pipe is
/// answered up front so the Linux close prompt cannot block the UI.
fn start_backend(
    args: &[OsString],
    entrypoint: Option<usize>,
    tx: &mpsc::Sender<String>,
) -> io::Result<(Child, ChildStdin)> {
    let mut child = spawn_backend(backend_run_args(args, entrypoint))?;
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| io::Error::other("backend stdin unavailable"))?;
    let _ = stdin.write_all(b"k\n");
    let _ = stdin.flush();
    if let Some(pipe) = child.stdout.take() {
        let sender = tx.clone();
        thread::spawn(move || pump(pipe, sender));
    }
    if let Some(pipe) = child.stderr.take() {
        let sender = tx.clone();
        thread::spawn(move || pump(pipe, sender));
    }
    Ok((child, stdin))
}

fn stop_child(child: &mut Option<Child>, stdin: &mut Option<ChildStdin>) {
    let _ = stdin.take();
    if let Some(child) = child.as_mut() {
        let _ = child.kill();
        let _ = child.wait();
    }
    *child = None;
}

/// Read byte by byte so a prompt that does not end with a newline is still
/// visible to the dashboard (both backends print their prompt with `print!`).
fn pump<R: Read + Send + 'static>(pipe: R, tx: mpsc::Sender<String>) {
    let mut partial: Vec<u8> = Vec::new();
    let mut byte = [0u8; 1];
    let mut pipe = pipe;
    loop {
        match pipe.read(&mut byte) {
            Ok(0) | Err(_) => break,
            Ok(_) if byte[0] == b'\n' => {
                let line = String::from_utf8_lossy(&partial).into_owned();
                partial.clear();
                if tx.send(line).is_err() {
                    break;
                }
            }
            Ok(_) => {
                partial.push(byte[0]);
                let looks_like_prompt = {
                    let text = String::from_utf8_lossy(&partial);
                    text.contains("Keep & Promote instead of destroying?")
                        || text.contains("before continuing?")
                };
                if looks_like_prompt {
                    let text = String::from_utf8_lossy(&partial).into_owned();
                    partial.clear();
                    if tx.send(text).is_err() {
                        break;
                    }
                }
            }
        }
    }
    if !partial.is_empty() {
        let _ = tx.send(String::from_utf8_lossy(&partial).into_owned());
    }
}

fn push_line(lines: &mut Vec<String>, line: String) {
    lines.push(line);
    if lines.len() > MAX_LOG_LINES {
        lines.drain(..lines.len() - MAX_LOG_LINES);
    }
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
    const WINDOWS: [&str; 4] = [
        "winorchestrator.exe",
        "win-orchestrator/target/release/winorchestrator.exe",
        "win-orchestrator/target/debug/winorchestrator.exe",
        "win-orchestrator/target/debug/cli.exe",
    ];
    let names: &[&str] = if cfg!(windows) { &WINDOWS } else { &LINUX };
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));

    for name in names {
        for candidate in [PathBuf::from(name), root.join(name), cwd.join(name)] {
            if candidate.is_file() {
                // A bare name would make Command search PATH again; anchor it.
                return Some(
                    if candidate.parent().is_some_and(|p| p.as_os_str().is_empty()) {
                        cwd.join(candidate)
                    } else {
                        candidate
                    },
                );
            }
        }
        if let Some(found) = find_in_path(name) {
            return Some(found);
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

// ---------------------------------------------------------------------------
// Dashboard data.
// ---------------------------------------------------------------------------

fn app_root() -> PathBuf {
    if cfg!(windows) {
        std::env::var_os("LOCALAPPDATA")
            .or_else(|| {
                std::env::var_os("USERPROFILE").map(|p| {
                    PathBuf::from(p)
                        .join("AppData")
                        .join("Local")
                        .into_os_string()
                })
            })
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir)
            .join("WinOrchestrator")
    } else {
        std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir)
            .join(".local")
            .join("share")
            .join("LinuxOrchestrator")
    }
}

fn sanitize(line: &str) -> String {
    line.chars()
        .filter(|ch| !ch.is_control())
        .collect::<String>()
        .trim_end()
        .to_string()
}

fn truncate(text: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    if text.chars().count() <= width {
        return text.to_string();
    }
    text.chars().take(width).collect()
}

fn progress_from(logs: &[String], finished: bool) -> f64 {
    if finished {
        return 1.0;
    }
    // Markers from both the Linux and the Windows backend.
    const STAGES: [(&str, f64); 19] = [
        ("Cloning ", 0.06),
        ("Cache hit", 0.12),
        ("Reused the cached shallow clone", 0.12),
        ("deterministic inspection", 0.18),
        ("Languages:", 0.26),
        ("[analyze]", 0.26),
        ("Host & download plan", 0.36),
        ("[probe]", 0.36),
        ("Preparing the sandbox", 0.48),
        ("[provision]", 0.50),
        ("Downloading ", 0.60),
        ("[sandbox] staged", 0.72),
        ("  root:", 0.72),
        ("[launch]", 0.82),
        ("  launch:", 0.82),
        ("Sandbox pid", 0.88),
        ("[run] executing", 0.90),
        ("Process exited with", 0.96),
        ("Program console closed", 0.96),
    ];
    let mut progress = 0.02;
    for line in logs {
        for (needle, value) in STAGES {
            if line.contains(needle) && value > progress {
                progress = value;
            }
        }
    }
    progress
}

fn collect_dependencies(logs: &[String]) -> Vec<DependencyEntry> {
    let mut deps: Vec<DependencyEntry> = Vec::new();
    let mut seen = std::collections::HashSet::new();

    let mut add = |label: String| {
        let label = label.trim().to_string();
        if label.is_empty() || !seen.insert(label.clone()) || deps.len() >= MAX_DEPS {
            return;
        }
        let runtime = runtime_token(&label);
        let promotable = (label.starts_with("download:")
            || label.starts_with("host:")
            || label.starts_with("promoted:"))
            && runtime.is_some();
        deps.push(DependencyEntry {
            label,
            promotable,
            runtime,
        });
    };

    for line in logs {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("Downloading ") {
            add(format!("download: {}", rest.trim_end_matches("...").trim()));
        } else if trimmed.starts_with("Injected host ") {
            add(format!(
                "host: {}",
                trimmed.trim_start_matches("Injected host ")
            ));
        } else if let Some(index) = trimmed.find("[provision] installing dependencies:") {
            let list = &trimmed[index + "[provision] installing dependencies:".len()..];
            for item in list.trim().trim_matches(['[', ']']).split(',') {
                let item = item.trim().trim_matches(['"', '\'']);
                if !item.is_empty() {
                    add(format!("package: {item}"));
                }
            }
        } else if trimmed.starts_with("promoted:") || trimmed.starts_with("PATH:") {
            add(trimmed.to_string());
        } else if trimmed.starts_with("Dependencies:") && trimmed.contains("declared") {
            add(format!(
                "declared: {}",
                trimmed.trim_start_matches("Dependencies:")
            ));
        } else if trimmed.contains("installed declared Python dependencies")
            || trimmed.contains("installed package.json dependencies")
            || trimmed.starts_with("- npm install")
        {
            add(format!(
                "package: {}",
                trimmed.trim_start_matches("- ").trim()
            ));
        }
    }
    deps
}

fn runtime_token(label: &str) -> Option<String> {
    let lower = label.to_ascii_lowercase();
    const TOKENS: [(&str, &str); 8] = [
        ("python", "python"),
        ("nodejs", "node"),
        ("node", "node"),
        ("rust", "rust"),
        ("golang", "go"),
        ("go-", "go"),
        ("java", "java"),
        ("jdk", "java"),
    ];
    TOKENS
        .iter()
        .find(|(needle, _)| lower.contains(needle))
        .map(|(_, canonical)| (*canonical).to_string())
}

fn runtime_matches(dir_name: &str, token: &str) -> bool {
    let lower = dir_name.to_ascii_lowercase();
    match token {
        "node" => lower.contains("node") || lower.contains("nodejs"),
        "go" => lower.contains("go") || lower.contains("golang"),
        "java" => lower.contains("java") || lower.contains("jdk"),
        _ => lower.contains(token),
    }
}

fn sync_dep_selection(deps: &[DependencyEntry], selected: &mut Vec<bool>, dep_cursor: &mut usize) {
    let previous: std::collections::HashMap<&str, bool> = deps
        .iter()
        .zip(selected.iter())
        .map(|(dep, &flag)| (dep.label.as_str(), flag))
        .collect();
    *selected = deps
        .iter()
        .map(|dep| {
            previous
                .get(dep.label.as_str())
                .copied()
                .unwrap_or(dep.promotable)
        })
        .collect();
    *dep_cursor = (*dep_cursor).min(deps.len().saturating_sub(1));
}

fn toggle_dependency(
    deps: &[DependencyEntry],
    selected: &mut [bool],
    cursor: usize,
) -> Option<String> {
    let dep = deps.get(cursor)?;
    if !dep.promotable {
        return Some(format!(
            "{} cannot be promoted to a system runtime.",
            dep.label
        ));
    }
    let flag = selected.get_mut(cursor)?;
    *flag = !*flag;
    Some(format!(
        "{} {}",
        if *flag { "Marked" } else { "Unmarked" },
        dep.label
    ))
}

fn selected_promotion_names(deps: &[DependencyEntry], selected: &[bool]) -> Vec<String> {
    let mut names: Vec<String> = deps
        .iter()
        .zip(selected.iter())
        .filter(|(dep, &flag)| dep.promotable && flag)
        .filter_map(|(dep, _)| dep.runtime.clone())
        .collect();
    names.sort();
    names.dedup();
    names
}

const ENTRYPOINT_PATHS: [&str; 17] = [
    "main.py",
    "app.py",
    "run.py",
    "manage.py",
    "server.py",
    "bot.py",
    "src/main.py",
    "src/app.py",
    "src/run.py",
    "index.js",
    "server.js",
    "app.js",
    "main.js",
    "src/index.js",
    "src/server.js",
    "src/main.js",
    "src/app.js",
];

fn entrypoint_candidates(files: &[String]) -> Vec<String> {
    let mut candidates: Vec<String> = ENTRYPOINT_PATHS
        .iter()
        .filter(|path| files.iter().any(|file| file == **path))
        .map(|path| (*path).to_string())
        .collect();
    if files.iter().any(|file| file == "src/main.rs") {
        candidates.push("src/main.rs".to_string());
    }
    for path in files {
        let normalized = path.replace('\\', "/");
        if normalized.ends_with("/main.go") || normalized == "main.go" {
            candidates.push(path.clone());
        }
    }
    let mut seen = std::collections::HashSet::new();
    candidates.retain(|path| seen.insert(path.clone()));
    candidates
}

fn is_entrypoint_candidate(file: &str) -> bool {
    ENTRYPOINT_PATHS.contains(&file)
        || file == "src/main.rs"
        || file.replace('\\', "/").ends_with("/main.go")
        || file == "main.go"
}

fn entrypoint_index(files: &[String], path: &str) -> Option<usize> {
    entrypoint_candidates(files)
        .iter()
        .position(|candidate| candidate == path)
        .map(|index| index + 1)
}

fn collect_files(target: &str) -> Vec<String> {
    let mut roots: Vec<PathBuf> = Vec::new();
    let direct = Path::new(target);
    if direct.is_dir() {
        roots.push(direct.to_path_buf());
    }

    let root = app_root();
    let workspaces = root.join("cache").join("workspaces");
    let dirs = recent_dirs(&workspaces);
    for dir in &dirs {
        let marker_text = fs::read_to_string(dir.join(".linuxorchestrator.json"))
            .or_else(|_| fs::read_to_string(dir.join(".winorchestrator.json")))
            .unwrap_or_default();
        if marker_text.contains(target) {
            roots.push(dir.clone());
            break;
        }
    }
    if roots.is_empty() {
        if let Some(dir) = dirs.first() {
            if dir.join(".git").is_dir() {
                roots.push(dir.clone());
            }
        }
    }

    let sandboxes = recent_dirs(&root.join("sandboxes"));
    for dir in sandboxes {
        let workspace = dir.join("workspace");
        if workspace.is_dir() {
            roots.push(workspace);
            break;
        }
    }

    let mut files = Vec::new();
    if let Some(repo) = roots.first() {
        walk_files(repo, "", &mut files, 0);
    }
    files
}

fn recent_dirs(path: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(path) else {
        return Vec::new();
    };
    let mut found: Vec<(SystemTime, PathBuf)> = entries
        .filter_map(|entry| {
            let entry = entry.ok()?;
            if entry.path().is_dir() {
                Some((entry.metadata().ok()?.modified().ok()?, entry.path()))
            } else {
                None
            }
        })
        .collect();
    found.sort_by_key(|(time, _)| Reverse(*time));
    found.into_iter().map(|(_, path)| path).collect()
}

fn walk_files(root: &Path, prefix: &str, out: &mut Vec<String>, depth: usize) {
    if out.len() >= MAX_FILES || depth > 12 {
        return;
    }
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    let mut paths: Vec<PathBuf> = entries
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .collect();
    paths.sort();
    for path in paths {
        let name = path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        if matches!(
            name.as_str(),
            ".git"
                | ".venv"
                | "venv"
                | "node_modules"
                | "target"
                | "__pycache__"
                | ".linuxorchestrator"
                | ".winorchestrator"
        ) || name == "sandbox.json"
        {
            continue;
        }
        let relative = if prefix.is_empty() {
            name
        } else {
            format!("{prefix}/{name}")
        };
        if path.is_dir() {
            walk_files(&path, &relative, out, depth + 1);
        } else if out.len() < MAX_FILES {
            out.push(relative);
        }
    }
}

// ---------------------------------------------------------------------------
// Promote to system: move sandbox downloads out of the sandbox and register
// them on the user PATH.
// ---------------------------------------------------------------------------

fn snapshot_dir_names(dir: &Path) -> Vec<String> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .filter_map(|entry| {
            let entry = entry.ok()?;
            if entry.path().is_dir() {
                Some(entry.file_name().to_string_lossy().into_owned())
            } else {
                None
            }
        })
        .collect();
    names.sort();
    names
}

/// Locate the current sandbox from either backend's log output.
fn find_sandbox_root(logs: &[String]) -> Option<PathBuf> {
    // Windows prints `  root:      C:\...\sandboxes\<id>`.
    if let Some(line) = logs
        .iter()
        .rev()
        .find(|line| line.trim_start().starts_with("root:"))
    {
        let path = line.trim_start().trim_start_matches("root:").trim();
        if !path.is_empty() {
            return Some(PathBuf::from(path));
        }
    }
    // Linux prints `[sandbox] staged <sandbox>/workspace`.
    if let Some(line) = logs
        .iter()
        .rev()
        .find(|line| line.contains("[sandbox] staged "))
    {
        let marker = "[sandbox] staged ";
        let start = line.find(marker)? + marker.len();
        let workspace = PathBuf::from(line[start..].trim());
        if let Some(parent) = workspace.parent() {
            return Some(parent.to_path_buf());
        }
    }
    recent_dirs(&app_root().join("sandboxes"))
        .into_iter()
        .next()
}

fn backend_promoted(logs: &[String]) -> bool {
    logs.iter().any(|line| {
        let line = line.trim();
        line.starts_with("promoted:") || line.starts_with("PATH:")
    })
}

/// Apply the decision made in the Dependencies tab. An empty `promote_names`
/// means discard everything; otherwise only the marked runtimes are promoted.
fn finalize_sandbox(logs: &[String], before: &[String], promote_names: &[String]) -> String {
    let toolchains = app_root().join("toolchains");
    let new_dirs: Vec<PathBuf> = snapshot_dir_names(&toolchains)
        .into_iter()
        .filter(|name| !before.contains(name))
        .map(|name| toolchains.join(name))
        .filter(|path| path.is_dir())
        .collect();
    let sandbox_root = find_sandbox_root(logs);

    if promote_names.is_empty() {
        for dir in &new_dirs {
            let _ = fs::remove_dir_all(dir);
        }
        if let Some(root) = &sandbox_root {
            let _ = fs::remove_dir_all(root);
        }
        return "Sandbox discarded; sandbox downloads reclaimed.".to_string();
    }

    if backend_promoted(logs) {
        // Windows (`--keep --no-update-path`) has already moved every runtime
        // into the system toolchain directory. Keep only the marked ones.
        let (mut selected, unselected): (Vec<PathBuf>, Vec<PathBuf>) = new_dirs
            .into_iter()
            .partition(|dir| dir_matches_names(dir, promote_names));
        for dir in &unselected {
            let _ = fs::remove_dir_all(dir);
        }
        selected.sort();
        selected.dedup();
        remove_matching_from_sandbox(sandbox_root.as_deref(), promote_names);
        let updated = update_user_path(&path_dirs(&selected));
        return format!(
            "Promoted {} marked runtime(s) to system; {} unmarked removed.{}",
            selected.len(),
            unselected.len(),
            if updated {
                " PATH registered (open a new terminal to use it)."
            } else {
                " PATH was already registered."
            }
        );
    }

    // Linux still owns its sandbox, so move only marked runtimes out of it.
    promote_from_sandbox(logs, &new_dirs, promote_names)
}

fn dir_matches_names(path: &Path, names: &[String]) -> bool {
    let name = path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();
    names.iter().any(|token| runtime_matches(&name, token))
}

fn remove_matching_from_sandbox(root: Option<&Path>, names: &[String]) {
    let Some(runtimes) = root.map(|root| root.join("runtimes")) else {
        return;
    };
    let Ok(entries) = fs::read_dir(&runtimes) else {
        return;
    };
    for entry in entries.filter_map(|entry| entry.ok()) {
        if dir_matches_names(&entry.path(), names) {
            let _ = fs::remove_dir_all(entry.path());
        }
    }
}

fn path_dirs(dirs: &[PathBuf]) -> Vec<PathBuf> {
    let mut result: Vec<PathBuf> = dirs
        .iter()
        .map(|dir| {
            let bin = dir.join("bin");
            if bin.is_dir() {
                bin
            } else {
                dir.clone()
            }
        })
        .collect();
    result.sort();
    result.dedup();
    result
}

fn promote_from_sandbox(logs: &[String], new_dirs: &[PathBuf], names: &[String]) -> String {
    let root = app_root();
    let toolchains = root.join("toolchains");
    let sandbox_root = find_sandbox_root(logs);

    let mut sources: Vec<PathBuf> = new_dirs.to_vec();
    if let Some(ref sandbox) = sandbox_root {
        sources.push(sandbox.join("runtimes"));
    }
    // Linux provisions downloads here before staging the sandbox.
    sources.push(toolchains.join("run"));

    let mut promoted: Vec<PathBuf> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for source in sources {
        let Ok(entries) = fs::read_dir(&source) else {
            continue;
        };
        for entry in entries.filter_map(|entry| entry.ok()) {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let name = path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
            if matches!(
                name.as_str(),
                "run" | "bin" | "lib" | "Scripts" | "site-packages"
            ) || !names.iter().any(|token| runtime_matches(&name, token))
            {
                continue;
            }
            let destination = toolchains.join(&name);
            if path != destination && copy_tree(&path, &destination).is_ok() {
                let _ = fs::remove_dir_all(&path);
            }
            if destination.is_dir() && seen.insert(destination.clone()) {
                promoted.push(destination);
            }
        }
    }

    if promoted.is_empty() {
        return "Nothing to promote: no marked sandbox downloads were found.".to_string();
    }
    let updated = update_user_path(&path_dirs(&promoted));
    format!(
        "Promoted {} marked runtime(s) to system; removed from sandbox.{}",
        promoted.len(),
        if updated {
            " PATH registered (open a new terminal to use it)."
        } else {
            " PATH was already registered or could not be updated."
        }
    )
}

fn copy_tree(source: &Path, destination: &Path) -> io::Result<()> {
    fs::create_dir_all(destination)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let path = entry.path();
        let target = destination.join(entry.file_name());
        if path.is_dir() {
            copy_tree(&path, &target)?;
        } else {
            fs::copy(&path, &target)?;
        }
    }
    Ok(())
}

fn update_user_path(dirs: &[PathBuf]) -> bool {
    if dirs.is_empty() {
        return false;
    }
    let additions: Vec<String> = dirs.iter().map(|dir| dir.display().to_string()).collect();

    #[cfg(windows)]
    {
        let current = std::env::var("PATH").unwrap_or_default();
        let mut parts: Vec<String> = current
            .split(';')
            .filter(|part| !part.is_empty())
            .map(str::to_string)
            .collect();
        for dir in &additions {
            if !parts.iter().any(|part| part.eq_ignore_ascii_case(dir)) {
                parts.insert(0, dir.clone());
            }
        }
        let joined = parts.join(";");
        Command::new("reg.exe")
            .args([
                "add",
                "HKCU\\Environment",
                "/v",
                "Path",
                "/t",
                "REG_EXPAND_SZ",
                "/d",
                joined.as_str(),
                "/f",
            ])
            .status()
            .map(|status| status.success())
            .unwrap_or(false)
    }

    #[cfg(not(windows))]
    {
        let bashrc = std::env::var_os("HOME")
            .map(PathBuf::from)
            .map(|path| path.join(".bashrc"))
            .unwrap_or_else(|| PathBuf::from("/tmp/.bashrc"));
        let current = fs::read_to_string(&bashrc).unwrap_or_default();
        if current.contains(&additions.join(":")) {
            return false;
        }
        let block = format!(
            "\n# sand promote-to-system\nexport PATH=\"{}:$PATH\"\n",
            additions.join(":")
        );
        match fs::OpenOptions::new()
            .append(true)
            .create(true)
            .open(&bashrc)
        {
            Ok(mut file) => file.write_all(block.as_bytes()).is_ok(),
            Err(_) => false,
        }
    }
}

// ---------------------------------------------------------------------------
// Windows ANSI support.
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// Tests for the view helpers (no terminal required).
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_advances_with_stages_and_completes() {
        let logs = vec![
            "[analyze] example detected".to_string(),
            "[sandbox] staged /tmp/sandbox/workspace".to_string(),
        ];
        assert!(progress_from(&logs, false) >= 0.72);
        assert_eq!(progress_from(&logs, true), 1.0);
    }

    #[test]
    fn dependencies_are_extracted_and_deduplicated() {
        let logs = vec![
            "Downloading python-3.12.tar.gz...".to_string(),
            "Downloading python-3.12.tar.gz...".to_string(),
            "Injected host Node 24 from PATH (no download needed).".to_string(),
            "[provision] installing dependencies: [\"flask\", \"requests\"]".to_string(),
        ];
        let deps = collect_dependencies(&logs);
        assert_eq!(deps.len(), 4);
        assert!(deps
            .iter()
            .any(|dep| dep.label == "download: python-3.12.tar.gz" && dep.promotable));
        assert!(deps
            .iter()
            .any(|dep| dep.label == "package: flask" && !dep.promotable));
    }

    #[test]
    fn selective_promotion_names_are_filtered() {
        let logs = vec![
            "Downloading python-3.12.tar.gz...".to_string(),
            "Downloading node-v24.tar.gz...".to_string(),
            "[provision] installing dependencies: [\"flask\"]".to_string(),
        ];
        let deps = collect_dependencies(&logs);
        let selected = vec![true, false, false];
        assert_eq!(
            selected_promotion_names(&deps, &selected),
            vec!["python".to_string()]
        );
        let mut selected = vec![false, false, false];
        assert_eq!(
            toggle_dependency(&deps, &mut selected, 1),
            Some("Marked download: node-v24.tar.gz".to_string())
        );
        assert_eq!(
            selected_promotion_names(&deps, &selected),
            vec!["node".to_string()]
        );
    }

    #[test]
    fn files_tab_detects_multiple_entrypoints() {
        let files = vec![
            "README.md".to_string(),
            "main.py".to_string(),
            "src/main.py".to_string(),
            "index.js".to_string(),
        ];
        assert_eq!(
            entrypoint_candidates(&files),
            vec![
                "main.py".to_string(),
                "src/main.py".to_string(),
                "index.js".to_string()
            ]
        );
        assert_eq!(entrypoint_index(&files, "src/main.py"), Some(2));
        assert_eq!(entrypoint_index(&files, "README.md"), None);
    }

    #[test]
    fn progress_bar_repeats_a_dune_shape() {
        assert_eq!(repeat_to_width(DUNE_FILL, 3), "⢠⣶⣿");
        assert!(DUNE_ART
            .iter()
            .all(|line| line.chars().count() == DUNE_ART_WIDTH));
        assert_eq!(dune_visible_range(-24, 80), (24, 24, 0));
        assert_eq!(dune_visible_range(0, 80), (0, 24, 0));
        assert_eq!(dune_visible_range(70, 80), (0, 10, 70));
    }

    #[test]
    fn truncate_counts_characters_and_sanitize_strips_controls() {
        assert_eq!(truncate("abcdef", 3), "abc");
        assert_eq!(sanitize("a\u{1b}[31mb"), "a[31mb");
    }

    #[test]
    fn windows_stages_advance_progress() {
        let logs = vec![
            "deterministic inspection".to_string(),
            "  root:      C:\\Users\\me\\AppData\\Local\\WinOrchestrator\\sandboxes\\one"
                .to_string(),
            "  launch:    python main.py".to_string(),
        ];
        assert!(progress_from(&logs, false) >= 0.82);
    }

    #[test]
    fn windows_dependency_and_root_markers_are_recognised() {
        let logs = vec![
            "  - installed declared Python dependencies into the sandbox venv".to_string(),
            "  root:      C:\\Users\\me\\AppData\\Local\\WinOrchestrator\\sandboxes\\one"
                .to_string(),
        ];
        let deps = collect_dependencies(&logs);
        assert!(deps.iter().any(|entry| entry
            .label
            .starts_with("package: installed declared Python")));
        assert_eq!(
            find_sandbox_root(&logs),
            Some(PathBuf::from(
                "C:\\Users\\me\\AppData\\Local\\WinOrchestrator\\sandboxes\\one"
            ))
        );
    }

    #[test]
    fn small_windows_reserve_dashboard_rows() {
        // Prompt on the last row of a 24-row terminal: leave room for the dune.
        assert_eq!(scroll_needed(23, 24), 14);
        // Plenty of space already: no scroll.
        assert_eq!(scroll_needed(2, 24), 0);
        // Tiny window: never scroll the command off screen.
        assert_eq!(scroll_needed(1, 5), 1);
        assert_eq!(scroll_needed(0, 5), 0);
    }

    #[test]
    fn backend_args_keep_the_sandbox_and_forward_entrypoint() {
        let base = vec![
            OsString::from("run"),
            OsString::from("https://github.com/example.git"),
        ];
        let args = backend_run_args(&base, None);
        assert!(args.iter().any(|arg| arg == "--keep"));
        assert!(args.iter().any(|arg| arg == "--no-update-path"));

        let args = backend_run_args(&base, Some(2));
        #[cfg(windows)]
        assert!(args.iter().any(|arg| arg == "2"));
        #[cfg(not(windows))]
        assert!(args.iter().any(|arg| arg == "--entry=2"));

        let explicit = backend_run_args(
            &[
                OsString::from("run"),
                OsString::from("."),
                OsString::from("--discard"),
            ],
            Some(1),
        );
        assert!(!explicit.iter().any(|arg| arg == "--keep"));
    }
}
