use std::env;
use std::io;
use std::path::{Path, PathBuf};

use anyback_reader::archive::ArchiveReader;
use anyback_reader::markdown::{
    SavedObjectKind, convert_archive_object_to_markdown, save_archive_object,
};
use anyhow::Result;
use crossterm::{
    event::{self, Event},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{Terminal, backend::CrosstermBackend, widgets::TableState};
use ratatui_image::{picker::Picker, protocol::StatefulProtocol};

use super::index::{ArchiveIndex, ObjectEntry, SortState};
use super::keys::{KeyAction, map_key_with_input_mode};
use super::ui;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PanelFocus {
    Contents,
    Links,
    Preview,
    Properties,
    Metadata,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputMode {
    None,
    Search,
    Filter,
    SaveAs,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkRow {
    pub relation: &'static str,
    pub target_id: String,
    pub target_name: String,
}

pub struct App {
    pub index: ArchiveIndex,
    pub focus: PanelFocus,
    pub sort: SortState,
    pub table_state: TableState,
    pub link_state: TableState,
    pub meta_scroll: u16,
    pub preview_scroll: u16,
    pub properties_scroll: u16,
    pub should_quit: bool,
    pub show_help: bool,
    pub input_mode: InputMode,
    pub input_buffer: String,
    /// Byte offset of the cursor within `input_buffer`.
    pub input_cursor: usize,
    pub search_query: String,
    pub type_filter: Option<String>,
    pub history: Vec<String>,
    pub filtered_indices: Vec<usize>,
    pub image_picker: Option<Picker>,
    pub image_preview: Option<StatefulProtocol>,
    pub image_preview_key: Option<(String, String)>,
    pub image_preview_error: Option<String>,
    pub markdown_preview: Option<String>,
    pub markdown_preview_key: Option<String>,
    pub markdown_preview_error: Option<String>,
    pub last_save_dir: Option<PathBuf>,
    pub status_message: Option<String>,
    // (content_lines, viewport_height) set during rendering for scroll clamping.
    pub preview_scroll_limit: (u16, u16),
    pub properties_scroll_limit: (u16, u16),
    pub meta_scroll_limit: (u16, u16),
}

impl App {
    pub fn new(index: ArchiveIndex) -> Self {
        let mut table_state = TableState::default();
        if !index.entries.is_empty() {
            table_state.select(Some(0));
        }

        let mut app = Self {
            index,
            focus: PanelFocus::Contents,
            sort: SortState::default(),
            table_state,
            link_state: TableState::default(),
            meta_scroll: 0,
            preview_scroll: 0,
            properties_scroll: 0,
            should_quit: false,
            show_help: false,
            input_mode: InputMode::None,
            input_buffer: String::new(),
            input_cursor: 0,
            search_query: String::new(),
            type_filter: None,
            history: Vec::new(),
            filtered_indices: Vec::new(),
            image_picker: None,
            image_preview: None,
            image_preview_key: None,
            image_preview_error: None,
            markdown_preview: None,
            markdown_preview_key: None,
            markdown_preview_error: None,
            last_save_dir: None,
            status_message: None,
            preview_scroll_limit: (0, 0),
            properties_scroll_limit: (0, 0),
            meta_scroll_limit: (0, 0),
        };

        app.apply_filters(None);
        app
    }

    pub fn run(path: &Path) -> Result<()> {
        eprintln!("Loading archive...");
        let index = ArchiveIndex::build(path)?;

        let mut app = Self::new(index);

        enable_raw_mode()?;
        let mut stdout = io::stdout();
        execute!(stdout, EnterAlternateScreen)?;

        let backend = CrosstermBackend::new(stdout);
        let mut terminal = Terminal::new(backend)?;
        app.init_image_picker();

        let original_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            let _ = disable_raw_mode();
            let _ = execute!(io::stdout(), LeaveAlternateScreen);
            original_hook(info);
        }));

        let result = app.event_loop(&mut terminal);

        disable_raw_mode()?;
        execute!(terminal.backend_mut(), LeaveAlternateScreen)?;

        result
    }

    fn init_image_picker(&mut self) {
        // Query the terminal for best graphics protocol (Kitty, Sixel, iTerm2)
        // and font size for accurate pixel mapping. Falls back to halfblocks.
        let picker = Picker::from_query_stdio().unwrap_or_else(|_| Picker::halfblocks());
        self.image_picker = Some(picker);
    }

    fn event_loop(&mut self, terminal: &mut Terminal<CrosstermBackend<io::Stdout>>) -> Result<()> {
        loop {
            terminal.draw(|frame| ui::draw(frame, self))?;

            if event::poll(std::time::Duration::from_millis(250))?
                && let Event::Key(key) = event::read()?
            {
                let action = map_key_with_input_mode(key, self.input_mode != InputMode::None);
                self.handle_action(action);
            }

            if self.should_quit {
                return Ok(());
            }
        }
    }

    pub(crate) fn handle_action(&mut self, action: KeyAction) {
        if self.input_mode != InputMode::None {
            self.handle_input_action(action);
            return;
        }

        if self.show_help {
            match action {
                KeyAction::ToggleHelp | KeyAction::Dismiss => {
                    self.show_help = false;
                }
                KeyAction::Quit => {
                    self.show_help = false;
                    self.should_quit = true;
                }
                _ => {}
            }
            return;
        }

        match action {
            KeyAction::Quit => self.should_quit = true,
            KeyAction::ToggleHelp => self.show_help = true,
            KeyAction::StartSearch => {
                self.input_mode = InputMode::Search;
                self.input_buffer = self.search_query.clone();
                self.input_cursor = self.input_buffer.len();
            }
            KeyAction::StartFilter => {
                self.input_mode = InputMode::Filter;
                self.input_buffer = self.type_filter.clone().unwrap_or_default();
                self.input_cursor = self.input_buffer.len();
            }
            KeyAction::StartSaveAs => {
                self.begin_save_as();
            }
            KeyAction::Dismiss => {
                if !self.search_query.is_empty() || self.type_filter.is_some() {
                    self.search_query.clear();
                    self.type_filter = None;
                    self.apply_filters(None);
                }
            }
            KeyAction::NavigateBack => self.navigate_back(),
            KeyAction::FollowLink => self.follow_selected_link(),
            KeyAction::Noop
            | KeyAction::InputChar(_)
            | KeyAction::Backspace
            | KeyAction::CursorLeft
            | KeyAction::CursorRight => {}
            KeyAction::NextPanel => {
                self.focus = match self.focus {
                    PanelFocus::Contents => PanelFocus::Links,
                    PanelFocus::Links => PanelFocus::Preview,
                    PanelFocus::Preview => PanelFocus::Properties,
                    PanelFocus::Properties => PanelFocus::Metadata,
                    PanelFocus::Metadata => PanelFocus::Contents,
                };
                self.ensure_link_selection();
            }
            KeyAction::PrevPanel => {
                self.focus = match self.focus {
                    PanelFocus::Contents => PanelFocus::Metadata,
                    PanelFocus::Links => PanelFocus::Contents,
                    PanelFocus::Preview => PanelFocus::Links,
                    PanelFocus::Properties => PanelFocus::Preview,
                    PanelFocus::Metadata => PanelFocus::Properties,
                };
                self.ensure_link_selection();
            }
            KeyAction::JumpPanel(n) => {
                self.focus = match n {
                    1 => PanelFocus::Contents,
                    2 => PanelFocus::Links,
                    3 => PanelFocus::Preview,
                    4 => PanelFocus::Properties,
                    5 => PanelFocus::Metadata,
                    _ => self.focus,
                };
                self.ensure_link_selection();
            }
            KeyAction::MoveDown => self.handle_scroll_down(1),
            KeyAction::MoveUp => self.handle_scroll_up(1),
            KeyAction::PageDown => self.handle_scroll_down(20),
            KeyAction::PageUp => self.handle_scroll_up(20),
            KeyAction::JumpFirst => self.handle_jump_first(),
            KeyAction::JumpLast => self.handle_jump_last(),
            KeyAction::ToggleSort => {
                self.sort.column = self.sort.column.next();
                self.sort.ascending = true;
                self.apply_sort();
            }
            KeyAction::ReverseSort => {
                self.sort.ascending = !self.sort.ascending;
                self.apply_sort();
            }
        }
    }

    fn handle_input_action(&mut self, action: KeyAction) {
        match action {
            KeyAction::Quit => {
                self.input_mode = InputMode::None;
                self.should_quit = true;
            }
            KeyAction::Dismiss => {
                self.input_mode = InputMode::None;
                self.input_buffer.clear();
                self.input_cursor = 0;
            }
            KeyAction::Backspace => {
                if self.input_cursor > 0 {
                    // Find the previous char boundary
                    let prev = self.input_buffer[..self.input_cursor]
                        .char_indices()
                        .next_back()
                        .map_or(0, |(i, _)| i);
                    self.input_buffer.drain(prev..self.input_cursor);
                    self.input_cursor = prev;
                }
            }
            KeyAction::InputChar(c) => {
                self.input_buffer.insert(self.input_cursor, c);
                self.input_cursor += c.len_utf8();
            }
            KeyAction::CursorLeft => {
                if self.input_cursor > 0 {
                    self.input_cursor = self.input_buffer[..self.input_cursor]
                        .char_indices()
                        .next_back()
                        .map_or(0, |(i, _)| i);
                }
            }
            KeyAction::CursorRight => {
                if self.input_cursor < self.input_buffer.len() {
                    self.input_cursor += self.input_buffer[self.input_cursor..]
                        .chars()
                        .next()
                        .map_or(0, char::len_utf8);
                }
            }
            KeyAction::FollowLink => {
                let value = self.input_buffer.trim().to_string();
                match self.input_mode {
                    InputMode::Search => {
                        self.search_query = value;
                        self.apply_filters(None);
                    }
                    InputMode::Filter => {
                        self.type_filter = if value.is_empty() { None } else { Some(value) };
                        self.apply_filters(None);
                    }
                    InputMode::SaveAs => {
                        self.save_current_to_path(&value);
                    }
                    InputMode::None => {}
                }
                self.input_mode = InputMode::None;
                self.input_buffer.clear();
                self.input_cursor = 0;
            }
            KeyAction::ToggleHelp => {
                self.input_mode = InputMode::None;
                self.input_buffer.clear();
                self.input_cursor = 0;
                self.show_help = true;
            }
            _ => {}
        }
    }

    #[allow(clippy::cast_possible_truncation)]
    fn handle_scroll_down(&mut self, amount: usize) {
        match self.focus {
            PanelFocus::Contents => {
                let len = self.filtered_indices.len();
                if len == 0 {
                    return;
                }
                let current = self.table_state.selected().unwrap_or(0);
                let next = (current + amount).min(len - 1);
                self.table_state.select(Some(next));
                self.meta_scroll = 0;
                self.preview_scroll = 0;
                self.properties_scroll = 0;
                self.ensure_link_selection();
            }
            PanelFocus::Links => {
                let len = self.current_links().len();
                if len == 0 {
                    return;
                }
                let current = self.link_state.selected().unwrap_or(0);
                self.link_state
                    .select(Some((current + amount).min(len - 1)));
            }
            PanelFocus::Preview => {
                self.preview_scroll = clamp_scroll(
                    self.preview_scroll,
                    amount as u16,
                    self.preview_scroll_limit,
                );
            }
            PanelFocus::Properties => {
                self.properties_scroll = clamp_scroll(
                    self.properties_scroll,
                    amount as u16,
                    self.properties_scroll_limit,
                );
            }
            PanelFocus::Metadata => {
                self.meta_scroll =
                    clamp_scroll(self.meta_scroll, amount as u16, self.meta_scroll_limit);
            }
        }
    }

    #[allow(clippy::cast_possible_truncation)]
    fn handle_scroll_up(&mut self, amount: usize) {
        match self.focus {
            PanelFocus::Contents => {
                let current = self.table_state.selected().unwrap_or(0);
                self.table_state
                    .select(Some(current.saturating_sub(amount)));
                self.meta_scroll = 0;
                self.preview_scroll = 0;
                self.properties_scroll = 0;
                self.ensure_link_selection();
            }
            PanelFocus::Links => {
                let current = self.link_state.selected().unwrap_or(0);
                self.link_state.select(Some(current.saturating_sub(amount)));
            }
            PanelFocus::Preview => {
                self.preview_scroll = self.preview_scroll.saturating_sub(amount as u16);
            }
            PanelFocus::Properties => {
                self.properties_scroll = self.properties_scroll.saturating_sub(amount as u16);
            }
            PanelFocus::Metadata => {
                self.meta_scroll = self.meta_scroll.saturating_sub(amount as u16);
            }
        }
    }

    fn handle_jump_first(&mut self) {
        match self.focus {
            PanelFocus::Contents => {
                if !self.filtered_indices.is_empty() {
                    self.table_state.select(Some(0));
                    self.meta_scroll = 0;
                    self.preview_scroll = 0;
                    self.properties_scroll = 0;
                    self.ensure_link_selection();
                }
            }
            PanelFocus::Links => {
                if !self.current_links().is_empty() {
                    self.link_state.select(Some(0));
                }
            }
            PanelFocus::Preview => {
                self.preview_scroll = 0;
            }
            PanelFocus::Properties => {
                self.properties_scroll = 0;
            }
            PanelFocus::Metadata => {
                self.meta_scroll = 0;
            }
        }
    }

    fn handle_jump_last(&mut self) {
        match self.focus {
            PanelFocus::Contents => {
                let len = self.filtered_indices.len();
                if len > 0 {
                    self.table_state.select(Some(len - 1));
                    self.meta_scroll = 0;
                    self.preview_scroll = 0;
                    self.properties_scroll = 0;
                    self.ensure_link_selection();
                }
            }
            PanelFocus::Links => {
                let len = self.current_links().len();
                if len > 0 {
                    self.link_state.select(Some(len - 1));
                }
            }
            PanelFocus::Preview => {
                self.preview_scroll = self.preview_scroll.saturating_add(100);
            }
            PanelFocus::Properties => {
                self.properties_scroll = self.properties_scroll.saturating_add(100);
            }
            PanelFocus::Metadata => {
                self.meta_scroll = self.meta_scroll.saturating_add(100);
            }
        }
    }

    fn apply_sort(&mut self) {
        let selected_id = self.current_entry().map(|e| e.id.clone());

        self.index.sort(self.sort);
        self.apply_filters(selected_id.as_deref());
    }

    fn ensure_link_selection(&mut self) {
        let links = self.current_links();
        if links.is_empty() {
            self.link_state.select(None);
            return;
        }
        let selected = self.link_state.selected().unwrap_or(0);
        self.link_state.select(Some(selected.min(links.len() - 1)));
    }

    fn follow_selected_link(&mut self) {
        if self.focus != PanelFocus::Links {
            return;
        }
        let links = self.current_links();
        let Some(selected) = self.link_state.selected() else {
            return;
        };
        let Some(target) = links.get(selected) else {
            return;
        };
        self.navigate_to_id(&target.target_id, true);
    }

    fn navigate_back(&mut self) {
        if let Some(id) = self.history.pop() {
            self.navigate_to_id(&id, false);
        }
    }

    fn navigate_to_id(&mut self, target_id: &str, push_history: bool) {
        if push_history
            && let Some(current) = self.current_entry().map(|e| e.id.clone())
            && current != target_id
        {
            self.history.push(current);
        }

        let visible_pos = self
            .filtered_indices
            .iter()
            .position(|&idx| self.index.entries[idx].id == target_id);
        if let Some(pos) = visible_pos {
            self.table_state.select(Some(pos));
            self.meta_scroll = 0;
            self.preview_scroll = 0;
            self.properties_scroll = 0;
            self.ensure_link_selection();
            return;
        }

        let target_exists = self.index.entries.iter().any(|entry| entry.id == target_id);
        if !target_exists {
            return;
        }

        self.search_query.clear();
        self.type_filter = None;
        self.apply_filters(Some(target_id));
    }

    fn apply_filters(&mut self, prefer_id: Option<&str>) {
        let query = self.search_query.to_ascii_lowercase();
        let type_filter = self.type_filter.as_ref().map(|v| v.to_ascii_lowercase());

        self.filtered_indices = self
            .index
            .entries
            .iter()
            .enumerate()
            .filter_map(|(idx, entry)| {
                let name_ok = query.is_empty() || entry.name.to_ascii_lowercase().contains(&query);
                let type_ok = type_filter.as_ref().is_none_or(|t| {
                    entry.type_display.to_ascii_lowercase().contains(t)
                        || entry
                            .type_name
                            .as_ref()
                            .is_some_and(|name| name.to_ascii_lowercase().contains(t))
                        || entry.type_id.to_ascii_lowercase().contains(t)
                        || entry.type_short_id.to_ascii_lowercase().contains(t)
                });
                if name_ok && type_ok { Some(idx) } else { None }
            })
            .collect();

        if self.filtered_indices.is_empty() {
            self.table_state.select(None);
            self.link_state.select(None);
            self.meta_scroll = 0;
            self.preview_scroll = 0;
            self.properties_scroll = 0;
            return;
        }

        let preferred = prefer_id
            .map(ToString::to_string)
            .or_else(|| self.current_entry().map(|e| e.id.clone()));

        let new_pos = preferred
            .as_deref()
            .and_then(|id| {
                self.filtered_indices
                    .iter()
                    .position(|&idx| self.index.entries[idx].id == id)
            })
            .unwrap_or(0);

        self.table_state.select(Some(new_pos));
        self.meta_scroll = 0;
        self.preview_scroll = 0;
        self.properties_scroll = 0;
        self.ensure_link_selection();
    }

    pub(crate) fn current_entry(&self) -> Option<&ObjectEntry> {
        let selected = self.table_state.selected()?;
        let idx = *self.filtered_indices.get(selected)?;
        self.index.entries.get(idx)
    }

    pub(crate) fn current_links(&self) -> Vec<LinkRow> {
        let Some(entry) = self.current_entry() else {
            return Vec::new();
        };

        let mut rows = Vec::new();
        for target in &entry.links {
            rows.push(LinkRow {
                relation: "link",
                target_id: target.clone(),
                target_name: self.lookup_name(target),
            });
        }
        for source in &entry.backlinks {
            rows.push(LinkRow {
                relation: "backlink",
                target_id: source.clone(),
                target_name: self.lookup_name(source),
            });
        }
        rows
    }

    fn lookup_name(&self, id: &str) -> String {
        self.index
            .entries
            .iter()
            .find(|entry| entry.id == id)
            .map_or_else(|| "(unknown)".to_string(), |entry| entry.name.clone())
    }

    pub(crate) fn prepare_image_preview(&mut self) {
        let selected = self.current_entry().map(|entry| {
            (
                entry.id.clone(),
                entry.image_payload_path.clone(),
                entry.file_mime.clone(),
            )
        });

        let Some((entry_id, image_path_opt, file_mime)) = selected else {
            self.image_preview = None;
            self.image_preview_key = None;
            self.image_preview_error = None;
            return;
        };
        let Some(image_path) = image_path_opt else {
            self.image_preview = None;
            self.image_preview_key = None;
            self.image_preview_error = file_mime.as_deref().and_then(|mime| {
                mime.starts_with("image/")
                    .then(|| "image payload not found in archive (use --include-files)".to_string())
            });
            return;
        };
        let key = (entry_id, image_path.clone());
        if self.image_preview_key.as_ref() == Some(&key) {
            return;
        }

        self.image_preview = None;
        self.image_preview_key = Some(key);
        self.image_preview_error = None;

        let Some(picker) = self.image_picker.as_ref() else {
            self.image_preview_error =
                Some("image preview unavailable (picker not initialized)".to_string());
            return;
        };

        let reader = match ArchiveReader::from_path(Path::new(&self.index.archive_path)) {
            Ok(reader) => reader,
            Err(err) => {
                self.image_preview_error = Some(format!("image preview unavailable: {err}"));
                return;
            }
        };
        let bytes = match reader.read_bytes(&image_path) {
            Ok(bytes) => bytes,
            Err(err) => {
                self.image_preview_error = Some(format!("failed to read image payload: {err}"));
                return;
            }
        };
        let image = match image::load_from_memory(&bytes) {
            Ok(image) => image,
            Err(err) => {
                self.image_preview_error = Some(format!("failed to decode image payload: {err}"));
                return;
            }
        };

        self.image_preview = Some(picker.new_resize_protocol(image));
    }

    pub(crate) fn prepare_markdown_preview(&mut self) {
        let selected = self
            .current_entry()
            .map(|entry| (entry.id.clone(), entry.layout_name.clone()));
        let Some((entry_id, layout_name)) = selected else {
            self.markdown_preview = None;
            self.markdown_preview_key = None;
            self.markdown_preview_error = None;
            return;
        };
        if layout_name.eq_ignore_ascii_case("image")
            || layout_name.eq_ignore_ascii_case("file")
            || layout_name.eq_ignore_ascii_case("audio")
            || layout_name.eq_ignore_ascii_case("video")
            || layout_name.eq_ignore_ascii_case("pdf")
        {
            self.markdown_preview = None;
            self.markdown_preview_key = None;
            self.markdown_preview_error = None;
            return;
        }
        if self.markdown_preview_key.as_deref() == Some(entry_id.as_str()) {
            return;
        }
        self.markdown_preview_key = Some(entry_id.clone());
        self.markdown_preview = None;
        self.markdown_preview_error = None;

        match convert_archive_object_to_markdown(Path::new(&self.index.archive_path), &entry_id) {
            Ok(markdown) if !markdown.trim().is_empty() => {
                self.markdown_preview = Some(markdown);
            }
            Ok(_) => {
                self.markdown_preview_error = Some("markdown preview unavailable".to_string());
            }
            Err(err) => {
                self.markdown_preview_error = Some(format!("markdown preview unavailable: {err}"));
            }
        }
    }

    fn begin_save_as(&mut self) {
        let Some(entry) = self.current_entry() else {
            self.status_message = Some("no object selected".to_string());
            return;
        };
        let base_dir = self
            .last_save_dir
            .clone()
            .or_else(|| env::current_dir().ok());
        let mut suggested = sanitize_save_name(&entry.name);
        if entry.layout_name.eq_ignore_ascii_case("image")
            || entry.layout_name.eq_ignore_ascii_case("file")
            || entry.layout_name.eq_ignore_ascii_case("audio")
            || entry.layout_name.eq_ignore_ascii_case("video")
            || entry.layout_name.eq_ignore_ascii_case("pdf")
        {
            let ext = Path::new(&entry.name)
                .extension()
                .and_then(|v| v.to_str())
                .map_or_else(|| ".bin".to_string(), |v| format!(".{v}"));
            suggested.push_str(&ext);
        } else {
            suggested.push_str(".md");
        }

        let full = base_dir
            .as_ref()
            .map_or_else(|| PathBuf::from(&suggested), |dir| dir.join(&suggested));
        self.input_mode = InputMode::SaveAs;
        self.input_buffer = full.display().to_string();
        self.input_cursor = self.input_buffer.len();
    }

    fn save_current_to_path(&mut self, value: &str) {
        if value.is_empty() {
            self.status_message = Some("save-as cancelled: empty path".to_string());
            return;
        }
        let Some(entry) = self.current_entry() else {
            self.status_message = Some("no object selected".to_string());
            return;
        };
        let path = PathBuf::from(value);
        let result = save_archive_object(Path::new(&self.index.archive_path), &entry.id, &path);
        match result {
            Ok(kind) => {
                if let Some(parent) = path.parent() {
                    self.last_save_dir = Some(parent.to_path_buf());
                }
                let label = match kind {
                    SavedObjectKind::Markdown => "markdown",
                    SavedObjectKind::Raw => "raw",
                };
                self.status_message = Some(format!("saved {label}: {}", path.display()));
            }
            Err(err) => {
                self.status_message = Some(format!("save failed: {err}"));
            }
        }
    }
}

/// Advance scroll by `amount`, clamped so the last line of content stays visible.
/// `limit` is `(content_lines, viewport_height)` recorded during the last render.
fn clamp_scroll(current: u16, amount: u16, (content_lines, viewport_height): (u16, u16)) -> u16 {
    let max_scroll = content_lines.saturating_sub(viewport_height);
    current.saturating_add(amount).min(max_scroll)
}

fn sanitize_save_name(name: &str) -> String {
    let mut out = String::new();
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
            out.push(ch);
        } else if ch.is_whitespace() || matches!(ch, '/' | '\\') {
            out.push('_');
        }
    }
    let trimmed = out.trim_matches('_');
    if trimmed.is_empty() {
        "object".to_string()
    } else {
        trimmed.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::inspector::index::{ObjectEntry, SortColumn};
    use anyback_reader::archive::ArchiveSourceKind;

    fn fixture_entry(id: &str, name: &str, type_name: &str) -> ObjectEntry {
        let short_id = if id.len() >= 5 {
            id[id.len() - 5..].to_string()
        } else {
            id.to_string()
        };
        ObjectEntry {
            id: id.to_string(),
            short_id,
            type_id: String::new(),
            type_short_id: String::new(),
            type_name: Some(type_name.to_string()),
            type_display: type_name.to_string(),
            name: name.to_string(),
            sb_type: "TEXT".to_string(),
            layout_name: "page".to_string(),
            created: "-".to_string(),
            modified: "-".to_string(),
            created_epoch: 0,
            modified_epoch: 0,
            size: 10,
            size_display: "10 B".to_string(),
            archived: false,
            path: format!("objects/{id}.pb"),
            readable: true,
            error: None,
            preview: format!("preview for {name}"),
            file_mime: None,
            image_payload_path: None,
            properties_count: 0,
            properties: Vec::new(),
            links: Vec::new(),
            backlinks: Vec::new(),
        }
    }

    fn fixture_app() -> App {
        let id_a = "bafyreiaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let id_b = "bafyreibbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
        let id_c = "bafyreiccccccccccccccccccccccccccccccccccccccccccccccccccccccc";

        let mut a = fixture_entry(id_a, "Alpha page", "page");
        let mut b = fixture_entry(id_b, "Beta note", "note");
        let mut c = fixture_entry(id_c, "Gamma page", "page");

        a.links = vec![id_b.to_string()];
        b.backlinks = vec![id_a.to_string()];
        c.links = vec![id_a.to_string()];

        let index = ArchiveIndex {
            archive_path: "fixture.abk".to_string(),
            source_kind: ArchiveSourceKind::Directory,
            manifest: None,
            manifest_error: None,
            file_count: 3,
            total_bytes: 30,
            format: "pb-json".to_string(),
            created_at: "-".to_string(),
            entries: vec![a, b, c],
        };

        App::new(index)
    }

    #[test]
    fn focus_cycle_and_panel_jumps_work() {
        let mut app = fixture_app();

        app.handle_action(KeyAction::NextPanel);
        assert_eq!(app.focus, PanelFocus::Links);
        app.handle_action(KeyAction::NextPanel);
        assert_eq!(app.focus, PanelFocus::Preview);
        app.handle_action(KeyAction::NextPanel);
        assert_eq!(app.focus, PanelFocus::Properties);
        app.handle_action(KeyAction::NextPanel);
        assert_eq!(app.focus, PanelFocus::Metadata);
        app.handle_action(KeyAction::NextPanel);
        assert_eq!(app.focus, PanelFocus::Contents);

        app.handle_action(KeyAction::JumpPanel(5));
        assert_eq!(app.focus, PanelFocus::Metadata);
        app.handle_action(KeyAction::JumpPanel(1));
        assert_eq!(app.focus, PanelFocus::Contents);
    }

    #[test]
    fn follow_link_and_back_navigation_restore_selection() {
        let mut app = fixture_app();
        let start = app.current_entry().unwrap().id.clone();

        app.handle_action(KeyAction::JumpPanel(2));
        app.handle_action(KeyAction::FollowLink);
        let after_follow = app.current_entry().unwrap().id.clone();
        assert_ne!(after_follow, start);
        assert_eq!(app.history.len(), 1);

        app.handle_action(KeyAction::NavigateBack);
        assert_eq!(app.current_entry().unwrap().id, start);
        assert!(app.history.is_empty());
    }

    #[test]
    fn search_and_filter_keyboard_workflow() {
        let mut app = fixture_app();

        app.handle_action(KeyAction::StartSearch);
        for ch in "beta".chars() {
            app.handle_action(KeyAction::InputChar(ch));
        }
        app.handle_action(KeyAction::FollowLink);
        assert_eq!(app.filtered_indices.len(), 1);
        assert_eq!(app.current_entry().unwrap().name, "Beta note");

        app.handle_action(KeyAction::StartFilter);
        for ch in "note".chars() {
            app.handle_action(KeyAction::InputChar(ch));
        }
        app.handle_action(KeyAction::FollowLink);
        assert_eq!(app.filtered_indices.len(), 1);

        app.handle_action(KeyAction::Dismiss);
        assert_eq!(app.filtered_indices.len(), 3);
    }

    #[test]
    fn sort_keeps_selected_object_when_possible() {
        let mut app = fixture_app();
        app.handle_action(KeyAction::MoveDown);
        let selected = app.current_entry().unwrap().id.clone();

        app.sort.column = SortColumn::Type;
        app.sort.ascending = false;
        app.apply_sort();

        assert_eq!(app.current_entry().unwrap().id, selected);
    }
}
