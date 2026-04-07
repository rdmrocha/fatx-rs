//! TUI file browser for FATX volumes.
//!
//! Single-pane browser with keyboard navigation:
//!   ↑/↓       Navigate file list
//!   Enter      Open directory / select file
//!   Backspace  Go up one directory
//!   d          Download selected file to local disk
//!   u          Upload a local file to current directory
//!   n          Create new directory
//!   D          Delete selected file/directory
//!   r          Rename selected file/directory
//!   i          Show volume info
//!   q/Esc      Quit

use std::fs;
use std::io::{self, stdout};
use std::path::PathBuf;

use crossterm::{
    event::{self, Event, KeyCode, KeyEvent},
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
    ExecutableCommand,
};
use ratatui::{
    prelude::*,
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph},
};

use fatxlib::partition::format_size;
use fatxlib::types::FileAttributes;
use fatxlib::volume::FatxVolume;

/// A directory entry for display in the TUI.
#[allow(dead_code)]
struct DisplayEntry {
    name: String,
    is_dir: bool,
    size: u64,
    modified: String,
    attributes: String,
    first_cluster: u32,
}

/// Application state for the TUI browser.
struct App {
    /// Current working directory path on the FATX volume.
    cwd: String,
    /// Entries in the current directory.
    entries: Vec<DisplayEntry>,
    /// Selection state for the list widget.
    list_state: ListState,
    /// Status message shown at the bottom.
    status: String,
    /// Whether to show the status as an error.
    status_is_error: bool,
    /// Partition name for display.
    partition_name: String,
    /// Device path for display.
    device_display: String,
    /// Whether app should quit.
    should_quit: bool,
    /// Input mode (for text prompts).
    input_mode: InputMode,
    /// Current text input buffer.
    input_buffer: String,
    /// Prompt text for input mode.
    input_prompt: String,
    /// Default download directory.
    download_dir: PathBuf,
}

#[derive(PartialEq)]
enum InputMode {
    Normal,
    DownloadPath,
    UploadPath,
    MkdirName,
    RenameName,
    ConfirmDelete,
}

impl App {
    fn new(partition_name: &str, device_display: &str) -> Self {
        let download_dir = dirs_or_home();
        Self {
            cwd: "/".to_string(),
            entries: Vec::new(),
            list_state: ListState::default(),
            status: "Loading...".to_string(),
            status_is_error: false,
            partition_name: partition_name.to_string(),
            device_display: device_display.to_string(),
            should_quit: false,
            input_mode: InputMode::Normal,
            input_buffer: String::new(),
            input_prompt: String::new(),
            download_dir,
        }
    }

    fn selected_entry(&self) -> Option<&DisplayEntry> {
        self.list_state.selected().and_then(|i| self.entries.get(i))
    }

    fn selected_name(&self) -> Option<String> {
        self.selected_entry().map(|e| e.name.clone())
    }

    fn set_status(&mut self, msg: &str) {
        self.status = msg.to_string();
        self.status_is_error = false;
    }

    fn set_error(&mut self, msg: &str) {
        self.status = msg.to_string();
        self.status_is_error = true;
    }

    fn full_path(&self, name: &str) -> String {
        if self.cwd == "/" {
            format!("/{}", name)
        } else {
            format!("{}/{}", self.cwd, name)
        }
    }
}

fn dirs_or_home() -> PathBuf {
    if let Ok(home) = std::env::var("HOME") {
        PathBuf::from(home).join("Desktop")
    } else {
        PathBuf::from(".")
    }
}

/// Refresh directory listing from the volume.
fn refresh_entries(app: &mut App, vol: &mut FatxVolume<std::fs::File>) {
    let entry = match vol.resolve_path(&app.cwd) {
        Ok(e) => e,
        Err(e) => {
            app.set_error(&format!("Error: {}", e));
            return;
        }
    };

    match vol.read_directory(entry.first_cluster) {
        Ok(entries) => {
            app.entries = entries
                .iter()
                .map(|e| {
                    let attr = format!(
                        "{}{}{}{}",
                        if e.is_directory() { "d" } else { "-" },
                        if e.attributes.contains(FileAttributes::READ_ONLY) { "r" } else { "-" },
                        if e.attributes.contains(FileAttributes::HIDDEN) { "h" } else { "-" },
                        if e.attributes.contains(FileAttributes::SYSTEM) { "s" } else { "-" },
                    );
                    DisplayEntry {
                        name: e.filename(),
                        is_dir: e.is_directory(),
                        size: e.file_size as u64,
                        modified: e.write_datetime_str(),
                        attributes: attr,
                        first_cluster: e.first_cluster,
                    }
                })
                .collect();

            // Sort: directories first, then alphabetical
            app.entries.sort_by(|a, b| {
                b.is_dir.cmp(&a.is_dir).then(a.name.to_lowercase().cmp(&b.name.to_lowercase()))
            });

            let count = app.entries.len();
            app.set_status(&format!("{} item(s) — ↑↓ navigate, Enter open, d download, u upload, q quit", count));

            // Reset selection
            if !app.entries.is_empty() {
                app.list_state.select(Some(0));
            } else {
                app.list_state.select(None);
                app.set_status("(empty directory) — Backspace to go up, u to upload, n to mkdir, q to quit");
            }
        }
        Err(e) => {
            app.set_error(&format!("Error reading directory: {}", e));
            app.entries.clear();
        }
    }
}

/// Main entry point — run the TUI browser.
pub fn run_browser(
    vol: &mut FatxVolume<std::fs::File>,
    partition_name: &str,
    device_display: &str,
) -> io::Result<()> {
    // Setup terminal
    enable_raw_mode()?;
    stdout().execute(EnterAlternateScreen)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(stdout()))?;

    let mut app = App::new(partition_name, device_display);
    refresh_entries(&mut app, vol);

    // Main loop
    loop {
        terminal.draw(|frame| ui(frame, &mut app))?;

        if app.should_quit {
            break;
        }

        if let Event::Key(key) = event::read()? {
            match app.input_mode {
                InputMode::Normal => handle_normal_key(&mut app, vol, key),
                _ => handle_input_key(&mut app, vol, key),
            }
        }
    }

    // Restore terminal
    disable_raw_mode()?;
    stdout().execute(LeaveAlternateScreen)?;

    Ok(())
}

/// Handle keys in normal browsing mode.
fn handle_normal_key(app: &mut App, vol: &mut FatxVolume<std::fs::File>, key: KeyEvent) {
    match key.code {
        KeyCode::Char('q') | KeyCode::Esc => {
            let _ = vol.flush();
            app.should_quit = true;
        }
        KeyCode::Up | KeyCode::Char('k') => {
            if let Some(sel) = app.list_state.selected() {
                if sel > 0 {
                    app.list_state.select(Some(sel - 1));
                }
            }
        }
        KeyCode::Down | KeyCode::Char('j') => {
            if let Some(sel) = app.list_state.selected() {
                if sel + 1 < app.entries.len() {
                    app.list_state.select(Some(sel + 1));
                }
            }
        }
        KeyCode::Home => {
            if !app.entries.is_empty() {
                app.list_state.select(Some(0));
            }
        }
        KeyCode::End => {
            if !app.entries.is_empty() {
                app.list_state.select(Some(app.entries.len() - 1));
            }
        }
        KeyCode::PageDown => {
            if let Some(sel) = app.list_state.selected() {
                let new = (sel + 20).min(app.entries.len().saturating_sub(1));
                app.list_state.select(Some(new));
            }
        }
        KeyCode::PageUp => {
            if let Some(sel) = app.list_state.selected() {
                let new = sel.saturating_sub(20);
                app.list_state.select(Some(new));
            }
        }
        KeyCode::Enter => {
            if let Some(entry) = app.selected_entry() {
                if entry.is_dir {
                    let new_cwd = app.full_path(&entry.name);
                    app.cwd = new_cwd;
                    refresh_entries(app, vol);
                } else {
                    // On Enter for a file, show info
                    let name = entry.name.clone();
                    let size = entry.size;
                    app.set_status(&format!("'{}' — {} — press 'd' to download", name, format_size(size)));
                }
            }
        }
        KeyCode::Backspace | KeyCode::Left => {
            // Go up one directory
            if app.cwd != "/" {
                if let Some(pos) = app.cwd.rfind('/') {
                    if pos == 0 {
                        app.cwd = "/".to_string();
                    } else {
                        app.cwd = app.cwd[..pos].to_string();
                    }
                    refresh_entries(app, vol);
                }
            }
        }
        // Download
        KeyCode::Char('d') => {
            let info = app.selected_entry().map(|e| (e.is_dir, e.name.clone()));
            if let Some((is_dir, name)) = info {
                if is_dir {
                    app.set_error("Cannot download a directory (select a file)");
                    return;
                }
                let default = app.download_dir.join(&name);
                app.input_buffer = default.to_string_lossy().to_string();
                app.input_prompt = format!("Download '{}' to:", name);
                app.input_mode = InputMode::DownloadPath;
            }
        }
        // Upload
        KeyCode::Char('u') => {
            app.input_buffer = app.download_dir.to_string_lossy().to_string();
            app.input_prompt = format!("Upload local file to '{}':", app.cwd);
            app.input_mode = InputMode::UploadPath;
        }
        // New directory
        KeyCode::Char('n') => {
            app.input_buffer.clear();
            app.input_prompt = "New directory name:".to_string();
            app.input_mode = InputMode::MkdirName;
        }
        // Delete
        KeyCode::Char('D') => {
            let info = app.selected_entry().map(|e| (e.is_dir, e.name.clone()));
            if let Some((is_dir, name)) = info {
                let kind = if is_dir { "directory" } else { "file" };
                app.input_prompt = format!("Delete {} '{}'? (y/n)", kind, name);
                app.input_buffer.clear();
                app.input_mode = InputMode::ConfirmDelete;
            }
        }
        // Rename
        KeyCode::Char('r') => {
            let name = app.selected_entry().map(|e| e.name.clone());
            if let Some(name) = name {
                app.input_buffer = name.clone();
                app.input_prompt = format!("Rename '{}' to:", name);
                app.input_mode = InputMode::RenameName;
            }
        }
        // Volume info
        KeyCode::Char('i') => {
            match vol.stats() {
                Ok(stats) => {
                    app.set_status(&format!(
                        "Volume: {} | Used: {} | Free: {} | Clusters: {}/{}",
                        app.partition_name,
                        format_size(stats.used_size),
                        format_size(stats.free_size),
                        stats.used_clusters,
                        stats.total_clusters,
                    ));
                }
                Err(e) => app.set_error(&format!("Stats error: {}", e)),
            }
        }
        _ => {}
    }
}

/// Handle keys in text input mode.
fn handle_input_key(app: &mut App, vol: &mut FatxVolume<std::fs::File>, key: KeyEvent) {
    match key.code {
        KeyCode::Esc => {
            app.input_mode = InputMode::Normal;
            app.set_status("Cancelled.");
        }
        KeyCode::Enter => {
            let input = app.input_buffer.clone();
            match app.input_mode {
                InputMode::DownloadPath => do_download(app, vol, &input),
                InputMode::UploadPath => do_upload(app, vol, &input),
                InputMode::MkdirName => do_mkdir(app, vol, &input),
                InputMode::RenameName => do_rename(app, vol, &input),
                InputMode::ConfirmDelete => {
                    if input.eq_ignore_ascii_case("y") || input.eq_ignore_ascii_case("yes") {
                        do_delete(app, vol);
                    } else {
                        app.set_status("Delete cancelled.");
                    }
                }
                InputMode::Normal => {}
            }
            app.input_mode = InputMode::Normal;
        }
        KeyCode::Backspace => {
            app.input_buffer.pop();
        }
        KeyCode::Char(c) => {
            // For confirm delete, just capture y/n
            if app.input_mode == InputMode::ConfirmDelete {
                app.input_buffer = c.to_string();
            } else {
                app.input_buffer.push(c);
            }
        }
        _ => {}
    }
}

fn do_download(app: &mut App, vol: &mut FatxVolume<std::fs::File>, local_path: &str) {
    let name = match app.selected_name() {
        Some(n) => n,
        None => { app.set_error("No file selected"); return; }
    };
    let fatx_path = app.full_path(&name);

    app.set_status(&format!("Downloading '{}'...", name));

    match vol.read_file_by_path(&fatx_path) {
        Ok(data) => {
            let path = PathBuf::from(local_path);
            // Create parent directories if needed
            if let Some(parent) = path.parent() {
                let _ = fs::create_dir_all(parent);
            }
            match fs::write(&path, &data) {
                Ok(_) => {
                    app.download_dir = path.parent().unwrap_or(&PathBuf::from(".")).to_path_buf();
                    app.set_status(&format!("Downloaded '{}' → {} ({})", name, path.display(), format_size(data.len() as u64)));
                }
                Err(e) => app.set_error(&format!("Write error: {}", e)),
            }
        }
        Err(e) => app.set_error(&format!("Read error: {}", e)),
    }
}

fn do_upload(app: &mut App, vol: &mut FatxVolume<std::fs::File>, local_path: &str) {
    let path = PathBuf::from(local_path);
    if !path.exists() {
        app.set_error(&format!("File not found: {}", local_path));
        return;
    }

    let filename = match path.file_name() {
        Some(n) => n.to_string_lossy().to_string(),
        None => { app.set_error("Invalid filename"); return; }
    };

    let fatx_path = app.full_path(&filename);
    app.set_status(&format!("Uploading '{}'...", filename));

    match fs::read(&path) {
        Ok(data) => {
            match vol.create_file(&fatx_path, &data) {
                Ok(_) => {
                    let _ = vol.flush();
                    app.download_dir = path.parent().unwrap_or(&PathBuf::from(".")).to_path_buf();
                    app.set_status(&format!("Uploaded '{}' → {} ({})", path.display(), fatx_path, format_size(data.len() as u64)));
                    refresh_entries(app, vol);
                }
                Err(e) => app.set_error(&format!("Upload error: {}", e)),
            }
        }
        Err(e) => app.set_error(&format!("Read local error: {}", e)),
    }
}

fn do_mkdir(app: &mut App, vol: &mut FatxVolume<std::fs::File>, name: &str) {
    if name.is_empty() {
        app.set_error("Name cannot be empty");
        return;
    }
    let path = app.full_path(name);
    match vol.create_directory(&path) {
        Ok(_) => {
            let _ = vol.flush();
            app.set_status(&format!("Created directory '{}'", name));
            refresh_entries(app, vol);
        }
        Err(e) => app.set_error(&format!("Mkdir error: {}", e)),
    }
}

fn do_delete(app: &mut App, vol: &mut FatxVolume<std::fs::File>) {
    let name = match app.selected_name() {
        Some(n) => n,
        None => { app.set_error("No file selected"); return; }
    };
    let path = app.full_path(&name);
    match vol.delete(&path) {
        Ok(_) => {
            let _ = vol.flush();
            app.set_status(&format!("Deleted '{}'", name));
            refresh_entries(app, vol);
        }
        Err(e) => app.set_error(&format!("Delete error: {}", e)),
    }
}

fn do_rename(app: &mut App, vol: &mut FatxVolume<std::fs::File>, new_name: &str) {
    let old_name = match app.selected_name() {
        Some(n) => n,
        None => { app.set_error("No file selected"); return; }
    };
    if new_name.is_empty() {
        app.set_error("Name cannot be empty");
        return;
    }
    let path = app.full_path(&old_name);
    match vol.rename(&path, new_name) {
        Ok(_) => {
            let _ = vol.flush();
            app.set_status(&format!("Renamed '{}' → '{}'", old_name, new_name));
            refresh_entries(app, vol);
        }
        Err(e) => app.set_error(&format!("Rename error: {}", e)),
    }
}

// ---------------------------------------------------------------------------
// UI rendering
// ---------------------------------------------------------------------------

fn ui(frame: &mut Frame, app: &mut App) {
    let area = frame.area();

    // Layout: header (3), file list (fill), status bar (3)
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),  // header
            Constraint::Min(5),    // file list
            Constraint::Length(3), // status / input
        ])
        .split(area);

    // -- Header --
    let header_text = format!(
        " 📦 {} — {} — {}",
        app.partition_name, app.device_display, app.cwd,
    );
    let header = Paragraph::new(header_text)
        .style(Style::default().fg(Color::White).bg(Color::DarkGray).bold())
        .block(Block::default().borders(Borders::BOTTOM).border_style(Style::default().fg(Color::Gray)));
    frame.render_widget(header, chunks[0]);

    // -- File list --
    let items: Vec<ListItem> = app
        .entries
        .iter()
        .map(|e| {
            let icon = if e.is_dir { "📁" } else { "📄" };
            let size_str = if e.is_dir {
                "<DIR>".to_string()
            } else {
                format_size(e.size)
            };
            let line = format!(
                " {} {:<42} {:>10}  {}  {}",
                icon, e.name, size_str, e.modified, e.attributes,
            );
            let style = if e.is_dir {
                Style::default().fg(Color::Cyan).bold()
            } else {
                Style::default().fg(Color::White)
            };
            ListItem::new(line).style(style)
        })
        .collect();

    let file_list = List::new(items)
        .block(
            Block::default()
                .title(" Files ")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Gray)),
        )
        .highlight_style(
            Style::default()
                .bg(Color::Rgb(40, 60, 100))
                .fg(Color::White)
                .bold(),
        )
        .highlight_symbol("▸ ");

    frame.render_stateful_widget(file_list, chunks[1], &mut app.list_state);

    // -- Status / Input bar --
    if app.input_mode != InputMode::Normal {
        // Input mode — show prompt + text field
        let input_text = format!(" {} {}", app.input_prompt, app.input_buffer);
        let input_bar = Paragraph::new(input_text)
            .style(Style::default().fg(Color::Yellow).bg(Color::Rgb(30, 30, 50)))
            .block(
                Block::default()
                    .title(" Input (Enter to confirm, Esc to cancel) ")
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::Yellow)),
            );
        frame.render_widget(input_bar, chunks[2]);
    } else {
        // Normal mode — show status
        let style = if app.status_is_error {
            Style::default().fg(Color::Red)
        } else {
            Style::default().fg(Color::Green)
        };
        let status_bar = Paragraph::new(format!(" {}", app.status))
            .style(style)
            .block(
                Block::default()
                    .title(" d:download  u:upload  n:mkdir  D:delete  r:rename  i:info  q:quit ")
                    .title_style(Style::default().fg(Color::DarkGray))
                    .borders(Borders::TOP)
                    .border_style(Style::default().fg(Color::Gray)),
            );
        frame.render_widget(status_bar, chunks[2]);
    }
}
