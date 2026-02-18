use std::collections::HashMap;
use std::env;
use std::fs;
use std::io;
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyback_reader::archive::ArchiveReader;
use anyback_reader::markdown::{
    ArchiveObjectInfo, SavedObjectKind, build_archive_object_index,
    convert_snapshot_bytes_to_markdown, save_archive_object,
};
use anyhow::{Result, anyhow};
use chrono::{DateTime, SecondsFormat, Utc};
use crossterm::{
    event::{self, Event},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use lru::LruCache;
use ratatui::{Terminal, backend::CrosstermBackend, widgets::TableState};
use ratatui_image::{picker::Picker, protocol::StatefulProtocol};
use serde_json::Value;

use super::index::{ArchiveIndex, ObjectEntry, SortState};
use super::keys::{KeyAction, map_key_with_input_mode};
use super::ui;
use crate::cli::decode::{parse_snapshot_details_from_pb, parse_snapshot_details_from_pb_json};

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

const DEFAULT_MAX_CACHE_BYTES: usize = 200 * 1024 * 1024;
const CACHE_LOW_WATERMARK_PCT: usize = 90;
const MIN_CACHE_ENTRY_BYTES: usize = 256 * 1024;

fn is_non_markdown_layout(layout_name: &str) -> bool {
    layout_name.eq_ignore_ascii_case("image")
        || layout_name.eq_ignore_ascii_case("file")
        || layout_name.eq_ignore_ascii_case("audio")
        || layout_name.eq_ignore_ascii_case("video")
        || layout_name.eq_ignore_ascii_case("pdf")
}

#[derive(Debug, Clone, Default)]
struct CachedObject {
    snapshot_bytes: Option<Vec<u8>>,
    markdown: Option<std::result::Result<String, String>>,
}

impl CachedObject {
    fn approx_bytes(&self) -> usize {
        let snapshot = self.snapshot_bytes.as_ref().map_or(0, Vec::len);
        let markdown = self.markdown.as_ref().map_or(0, |result| match result {
            Ok(text) | Err(text) => text.len(),
        });
        snapshot.saturating_add(markdown)
    }
}

struct ObjectCache {
    lru: LruCache<String, CachedObject>,
    current_bytes: usize,
    max_bytes: usize,
    max_entry_bytes: usize,
    low_watermark_bytes: usize,
}

impl ObjectCache {
    fn new(max_bytes: usize) -> Self {
        let max_bytes = max_bytes.max(1);
        let max_entry_bytes = (max_bytes / 5).max(MIN_CACHE_ENTRY_BYTES).min(max_bytes);
        let low_watermark_bytes = (max_bytes.saturating_mul(CACHE_LOW_WATERMARK_PCT)) / 100;
        Self {
            lru: LruCache::new(NonZeroUsize::new(4096).expect("non-zero cache capacity")),
            current_bytes: 0,
            max_bytes,
            max_entry_bytes,
            low_watermark_bytes,
        }
    }

    fn get_markdown(&mut self, object_id: &str) -> Option<std::result::Result<String, String>> {
        self.lru
            .get(object_id)
            .and_then(|entry| entry.markdown.clone())
    }

    fn get_snapshot_bytes(&mut self, object_id: &str) -> Option<Vec<u8>> {
        self.lru
            .get(object_id)
            .and_then(|entry| entry.snapshot_bytes.clone())
    }

    fn upsert(
        &mut self,
        object_id: String,
        snapshot_bytes: Option<Vec<u8>>,
        markdown: Option<std::result::Result<String, String>>,
    ) {
        let mut entry = if let Some(existing) = self.lru.pop(&object_id) {
            self.current_bytes = self.current_bytes.saturating_sub(existing.approx_bytes());
            existing
        } else {
            CachedObject::default()
        };

        if let Some(snapshot) = snapshot_bytes {
            entry.snapshot_bytes = Some(snapshot);
        }
        if let Some(markdown) = markdown {
            entry.markdown = Some(markdown);
        }

        let approx = entry.approx_bytes();
        if approx == 0 || approx > self.max_entry_bytes {
            return;
        }

        self.evict_for(approx);
        if self.current_bytes.saturating_add(approx) > self.max_bytes {
            return;
        }

        self.current_bytes = self.current_bytes.saturating_add(approx);
        self.lru.put(object_id, entry);
    }

    fn evict_for(&mut self, incoming_bytes: usize) {
        if self.current_bytes.saturating_add(incoming_bytes) <= self.max_bytes {
            return;
        }
        while self.current_bytes.saturating_add(incoming_bytes) > self.low_watermark_bytes {
            let Some((_, evicted)) = self.lru.pop_lru() else {
                break;
            };
            self.current_bytes = self.current_bytes.saturating_sub(evicted.approx_bytes());
        }
    }
}

pub struct App {
    pub index: ArchiveIndex,
    pub archive_reader: Option<ArchiveReader>,
    pub markdown_index: Option<HashMap<String, ArchiveObjectInfo>>,
    object_cache: ObjectCache,
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
    pending_open_editor: bool,
    // (content_lines, viewport_height) set during rendering for scroll clamping.
    pub preview_scroll_limit: (u16, u16),
    pub properties_scroll_limit: (u16, u16),
    pub meta_scroll_limit: (u16, u16),
    pub contents_visible_rows: u16,
    pub links_visible_rows: u16,
}

impl App {
    #[allow(dead_code)]
    pub fn new(index: ArchiveIndex) -> Self {
        Self::with_resources(index, None, None, DEFAULT_MAX_CACHE_BYTES)
    }

    pub fn with_resources(
        index: ArchiveIndex,
        archive_reader: Option<ArchiveReader>,
        markdown_index: Option<HashMap<String, ArchiveObjectInfo>>,
        max_cache_bytes: usize,
    ) -> Self {
        let mut table_state = TableState::default();
        if !index.entries.is_empty() {
            table_state.select(Some(0));
        }

        let mut app = Self {
            index,
            archive_reader,
            markdown_index,
            object_cache: ObjectCache::new(max_cache_bytes),
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
            pending_open_editor: false,
            preview_scroll_limit: (0, 0),
            properties_scroll_limit: (0, 0),
            meta_scroll_limit: (0, 0),
            contents_visible_rows: 0,
            links_visible_rows: 0,
        };

        app.apply_filters(None);
        app
    }

    pub fn run(path: &Path, max_cache_bytes: usize) -> Result<()> {
        eprintln!("Loading archive...");
        let archive_reader = ArchiveReader::from_path(path)?;
        let markdown_index = build_archive_object_index(&archive_reader).ok();
        let index = ArchiveIndex::build(path)?;

        let mut app = Self::with_resources(index, None, None, max_cache_bytes);
        app.archive_reader = Some(archive_reader);
        app.markdown_index = markdown_index;

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
                if self.pending_open_editor {
                    self.pending_open_editor = false;
                    if let Err(err) = self.open_current_in_editor(terminal) {
                        self.status_message = Some(format!("open editor failed: {err}"));
                    }
                }
            }

            if self.should_quit {
                return Ok(());
            }
        }
    }

    #[allow(clippy::too_many_lines)]
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
            KeyAction::CopyObjectId => self.copy_current_object_id(),
            KeyAction::OpenInAnytype => self.open_current_in_anytype(),
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
            KeyAction::OpenInEditor => {
                self.pending_open_editor = true;
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
            | KeyAction::CursorRight
            | KeyAction::CursorStart
            | KeyAction::CursorEnd
            | KeyAction::KillToEnd => {}
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
            KeyAction::HalfPageDown => self.handle_scroll_down(self.half_page_amount()),
            KeyAction::HalfPageUp => self.handle_scroll_up(self.half_page_amount()),
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
            KeyAction::CursorStart => {
                self.input_cursor = 0;
            }
            KeyAction::CursorEnd => {
                self.input_cursor = self.input_buffer.len();
            }
            KeyAction::KillToEnd => {
                self.input_buffer.truncate(self.input_cursor);
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

    fn half_page_amount(&self) -> usize {
        let viewport = match self.focus {
            PanelFocus::Contents => self.contents_visible_rows,
            PanelFocus::Links => self.links_visible_rows,
            PanelFocus::Preview => self.preview_scroll_limit.1,
            PanelFocus::Properties => self.properties_scroll_limit.1,
            PanelFocus::Metadata => self.meta_scroll_limit.1,
        };
        usize::from((viewport / 2).max(1))
    }

    fn copy_current_object_id(&mut self) {
        let Some(entry) = self.current_entry() else {
            self.status_message = Some("no object selected".to_string());
            return;
        };
        let id = entry.id.clone();
        match arboard::Clipboard::new() {
            Ok(mut clipboard) => match clipboard.set_text(id.clone()) {
                Ok(()) => {
                    self.status_message = Some(format!("copied object id: {id}"));
                }
                Err(err) => {
                    self.status_message = Some(format!("copy failed: {err}"));
                }
            },
            Err(err) => {
                self.status_message = Some(format!("copy failed: {err}"));
            }
        }
    }

    fn open_current_in_anytype(&mut self) {
        let Some(entry) = self.current_entry() else {
            self.status_message = Some("no object selected".to_string());
            return;
        };
        let object_id = entry.id.clone();
        let Some(space_id) = self.resolve_space_id() else {
            self.status_message = Some("open failed: could not resolve space id".to_string());
            return;
        };
        let url = format!("anytype://object?objectId={object_id}&spaceId={space_id}");
        match open_url(&url) {
            Ok(()) => {
                self.status_message = Some(format!("opened in Anytype: {object_id}"));
            }
            Err(err) => {
                self.status_message = Some(format!("open failed: {err}"));
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

        let Some(reader) = self.archive_reader.as_ref() else {
            self.image_preview_error =
                Some("image preview unavailable: archive reader not initialized".to_string());
            return;
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
        let selected = self.current_entry().map(|entry| {
            (
                entry.id.clone(),
                entry.layout_name.clone(),
                entry.path.clone(),
            )
        });
        let Some((entry_id, layout_name, entry_path)) = selected else {
            self.markdown_preview = None;
            self.markdown_preview_key = None;
            self.markdown_preview_error = None;
            return;
        };
        if is_non_markdown_layout(&layout_name) {
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

        if let Some(cached) = self.object_cache.get_markdown(&entry_id) {
            match cached {
                Ok(markdown) => self.markdown_preview = Some(markdown),
                Err(err) => self.markdown_preview_error = Some(err),
            }
            return;
        }

        let mut snapshot_bytes_to_cache = None;
        let snapshot_bytes = if let Some(cached) = self.object_cache.get_snapshot_bytes(&entry_id) {
            cached
        } else {
            let Some(reader) = self.archive_reader.as_ref() else {
                let msg =
                    "markdown preview unavailable: archive reader not initialized".to_string();
                self.markdown_preview_error = Some(msg.clone());
                self.object_cache.upsert(entry_id, None, Some(Err(msg)));
                return;
            };
            let bytes = match reader.read_bytes(&entry_path) {
                Ok(bytes) => bytes,
                Err(err) => {
                    let msg =
                        format!("markdown preview unavailable: failed to read snapshot: {err}");
                    self.markdown_preview_error = Some(msg.clone());
                    self.object_cache.upsert(entry_id, None, Some(Err(msg)));
                    return;
                }
            };
            snapshot_bytes_to_cache = Some(bytes.clone());
            bytes
        };

        let rendered = (|| -> Result<String> {
            let object_index = self
                .markdown_index
                .as_ref()
                .ok_or_else(|| anyhow!("markdown index not initialized"))?;
            convert_snapshot_bytes_to_markdown(&entry_path, &snapshot_bytes, object_index)
        })();

        match rendered {
            Ok(markdown) if !markdown.trim().is_empty() => {
                self.markdown_preview = Some(markdown.clone());
                self.object_cache
                    .upsert(entry_id, snapshot_bytes_to_cache, Some(Ok(markdown)));
            }
            Ok(_) => {
                let msg = "markdown preview unavailable".to_string();
                self.markdown_preview_error = Some(msg.clone());
                self.object_cache
                    .upsert(entry_id, snapshot_bytes_to_cache, Some(Err(msg)));
            }
            Err(err) => {
                let msg = format!("markdown preview unavailable: {err}");
                self.markdown_preview_error = Some(msg.clone());
                self.object_cache
                    .upsert(entry_id, snapshot_bytes_to_cache, Some(Err(msg)));
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
        if is_non_markdown_layout(&entry.layout_name) {
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
        let object_id = entry.id.clone();
        let layout_name = entry.layout_name.clone();
        let path = PathBuf::from(value);
        let result = if is_non_markdown_layout(&layout_name) {
            save_archive_object(Path::new(&self.index.archive_path), &object_id, &path)
        } else {
            self.write_current_markdown_to_path(&path)
                .map(|()| SavedObjectKind::Markdown)
        };
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

    fn open_current_in_editor(
        &mut self,
        terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    ) -> Result<()> {
        let Some(entry) = self.current_entry() else {
            return Err(anyhow!("no object selected"));
        };
        if is_non_markdown_layout(&entry.layout_name) {
            return Err(anyhow!(
                "selected object is a binary/file layout; editor open supports markdown objects only"
            ));
        }
        let object_id = entry.id.clone();
        let path = env::temp_dir().join(format!("{object_id}.md"));
        self.write_current_markdown_to_path(&path)?;

        let editor = env::var("EDITOR").map_err(|_| anyhow!("$EDITOR is not set"))?;

        disable_raw_mode().map_err(|err| anyhow!("failed to disable raw mode: {err}"))?;
        execute!(terminal.backend_mut(), LeaveAlternateScreen)
            .map_err(|err| anyhow!("failed to leave alternate screen: {err}"))?;

        let open_result = run_editor_command(&editor, &path);

        let restore_screen = execute!(terminal.backend_mut(), EnterAlternateScreen);
        let restore_raw = enable_raw_mode();
        if let Err(err) = restore_screen {
            return Err(anyhow!("failed to restore alternate screen: {err}"));
        }
        if let Err(err) = restore_raw {
            return Err(anyhow!("failed to re-enable raw mode: {err}"));
        }
        terminal
            .clear()
            .map_err(|err| anyhow!("failed to clear terminal after editor exit: {err}"))?;

        open_result?;
        self.status_message = Some(format!("edited: {}", path.display()));
        Ok(())
    }

    fn write_current_markdown_to_path(&self, dest: &Path) -> Result<()> {
        let Some(entry) = self.current_entry() else {
            return Err(anyhow!("no object selected"));
        };
        if is_non_markdown_layout(&entry.layout_name) {
            return Err(anyhow!("selected object cannot be exported as markdown"));
        }
        let object_id = entry.id.clone();
        let object_name = entry.name.clone();
        let object_type = entry.type_display.clone();
        let object_properties = entry.properties.clone();
        let snapshot_path = entry.path.clone();
        let name_by_id: HashMap<String, String> = self
            .index
            .entries
            .iter()
            .filter_map(|item| {
                let name = item.name.trim();
                (!name.is_empty()).then(|| (item.id.clone(), name.to_string()))
            })
            .collect();

        let reader = self
            .archive_reader
            .as_ref()
            .ok_or_else(|| anyhow!("archive reader not initialized"))?;
        let snapshot_bytes = reader
            .read_bytes(&snapshot_path)
            .map_err(|err| anyhow!("failed reading snapshot from archive: {err}"))?;
        let details = parse_snapshot_details_map(&snapshot_path, &snapshot_bytes)?;
        let markdown = if let Some(object_index) = self.markdown_index.as_ref() {
            convert_snapshot_bytes_to_markdown(&snapshot_path, &snapshot_bytes, object_index)?
        } else {
            let object_index = build_archive_object_index(reader)?;
            convert_snapshot_bytes_to_markdown(&snapshot_path, &snapshot_bytes, &object_index)?
        };

        let front_matter = build_yaml_front_matter(
            &object_id,
            &object_name,
            &object_type,
            self.index
                .manifest
                .as_ref()
                .map(|manifest| manifest.source_space_id.as_str()),
            &details,
            &name_by_id,
            &object_properties,
        );
        let output = format!("{front_matter}\n{markdown}");

        fs::write(dest, output)
            .map_err(|err| anyhow!("failed writing markdown to {}: {err}", dest.display()))?;
        Ok(())
    }

    fn resolve_space_id(&self) -> Option<String> {
        if let Some(manifest) = self.index.manifest.as_ref() {
            let space_id = manifest.source_space_id.trim();
            if !space_id.is_empty() {
                return Some(space_id.to_string());
            }
        }

        if let Some(entry) = self.current_entry()
            && let Some(reader) = self.archive_reader.as_ref()
            && let Ok(bytes) = reader.read_bytes(&entry.path)
            && let Ok(details) = parse_snapshot_details_map(&entry.path, &bytes)
        {
            let empty_index: HashMap<String, String> = HashMap::new();
            if let Some(space_id) =
                detail_string(&details, &["spaceId", "spaceID", "space_id"], &empty_index)
            {
                let trimmed = space_id.trim();
                if !trimmed.is_empty() {
                    return Some(trimmed.to_string());
                }
            }
        }

        extract_space_id_from_archive_name(Path::new(&self.index.archive_path))
    }
}

fn run_editor_command(editor: &str, file_path: &Path) -> Result<()> {
    let quoted_path = shell_quote_path(file_path);
    let cmdline = format!("{editor} {quoted_path}");
    #[cfg(windows)]
    let status = Command::new("cmd")
        .args(["/C", &cmdline])
        .status()
        .map_err(|err| anyhow!("failed to launch editor via cmd: {err}"))?;
    #[cfg(not(windows))]
    let status = Command::new("sh")
        .args(["-c", &cmdline])
        .status()
        .map_err(|err| anyhow!("failed to launch editor via sh: {err}"))?;

    if !status.success() {
        return Err(anyhow!("editor exited with status {status}"));
    }
    Ok(())
}

fn open_url(url: &str) -> Result<()> {
    #[cfg(target_os = "macos")]
    let mut command = {
        let mut cmd = Command::new("open");
        cmd.arg(url);
        cmd
    };
    #[cfg(target_os = "linux")]
    let mut command = {
        let mut cmd = Command::new("xdg-open");
        cmd.arg(url);
        cmd
    };
    #[cfg(target_os = "windows")]
    let mut command = {
        let mut cmd = Command::new("cmd");
        cmd.args(["/C", "start", "", url]);
        cmd
    };

    let status = command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|err| anyhow!("failed to launch url opener: {err}"))?;
    if !status.success() {
        return Err(anyhow!("url opener exited with status {status}"));
    }
    Ok(())
}

fn extract_space_id_from_archive_name(path: &Path) -> Option<String> {
    let file_name = path.file_name()?.to_str()?;
    for token in file_name.split(|c: char| !c.is_ascii_alphanumeric()) {
        if looks_like_object_id(token) {
            return Some(token.to_string());
        }
    }
    None
}

#[cfg(windows)]
fn shell_quote_path(path: &Path) -> String {
    let text = path.to_string_lossy().replace('"', "\"\"");
    format!("\"{text}\"")
}

#[cfg(not(windows))]
fn shell_quote_path(path: &Path) -> String {
    let text = path.to_string_lossy().replace('\'', "'\"'\"'");
    format!("'{text}'")
}

fn parse_snapshot_details_map(
    snapshot_path: &str,
    snapshot_bytes: &[u8],
) -> Result<serde_json::Map<String, Value>> {
    let lower = snapshot_path.to_ascii_lowercase();
    #[allow(clippy::case_sensitive_file_extension_comparisons)]
    if lower.ends_with(".pb.json") {
        return parse_snapshot_details_from_pb_json(snapshot_bytes).map(|(_, details)| details);
    }
    #[allow(clippy::case_sensitive_file_extension_comparisons)]
    if lower.ends_with(".pb") {
        return parse_snapshot_details_from_pb(snapshot_bytes).map(|(_, details)| details);
    }
    Err(anyhow!("unsupported snapshot format: {snapshot_path}"))
}

fn build_yaml_front_matter(
    object_id: &str,
    object_name: &str,
    object_type: &str,
    default_space_id: Option<&str>,
    details: &serde_json::Map<String, Value>,
    name_by_id: &HashMap<String, String>,
    properties: &[(String, String)],
) -> String {
    let name =
        detail_string(details, &["name"], name_by_id).unwrap_or_else(|| object_name.to_string());
    let space_id = detail_string(details, &["spaceId", "spaceID", "space_id"], name_by_id)
        .or_else(|| default_space_id.map(ToString::to_string))
        .unwrap_or_default();
    let object_type =
        detail_string(details, &["type"], name_by_id).unwrap_or_else(|| object_type.to_string());
    let creator = detail_string(details, &["creator", "createdBy", "created_by"], name_by_id)
        .unwrap_or_default();
    let created_date = detail_string(details, &["createdDate", "created_date"], name_by_id)
        .as_deref()
        .and_then(value_as_rfc3339)
        .unwrap_or_default();
    let last_modified_date = detail_string(
        details,
        &["lastModifiedDate", "last_modified_date"],
        name_by_id,
    )
    .as_deref()
    .and_then(value_as_rfc3339)
    .unwrap_or_default();

    let mut output = format!(
        concat!(
            "---\n",
            "name: {}\n",
            "object_id: {}\n",
            "space_id: {}\n",
            "type: {}\n",
            "created_date: {}\n",
            "last_modified_date: {}\n",
            "creator: {}\n"
        ),
        yaml_quote(&name),
        yaml_quote(object_id),
        yaml_quote(&space_id),
        yaml_quote(&object_type),
        yaml_quote(&created_date),
        yaml_quote(&last_modified_date),
        yaml_quote(&creator),
    );

    if !properties.is_empty() {
        output.push_str("properties:\n");
        for (key, value) in properties {
            output.push_str("  ");
            output.push_str(&yaml_key(key));
            output.push_str(": ");
            output.push_str(&yaml_quote(value));
            output.push('\n');
        }
    }

    output.push_str("---\n");
    output
}

fn detail_string(
    details: &serde_json::Map<String, Value>,
    keys: &[&str],
    name_by_id: &HashMap<String, String>,
) -> Option<String> {
    keys.iter()
        .find_map(|key| details.get(*key))
        .and_then(|value| detail_value_to_string(value, name_by_id))
}

fn detail_value_to_string(value: &Value, name_by_id: &HashMap<String, String>) -> Option<String> {
    match value {
        Value::String(text) => {
            let trimmed = text.trim();
            if trimmed.is_empty() {
                None
            } else if looks_like_object_id(trimmed) {
                Some(format_object_ref(trimmed, name_by_id))
            } else {
                Some(trimmed.to_string())
            }
        }
        Value::Number(num) => Some(num.to_string()),
        Value::Bool(v) => Some(v.to_string()),
        Value::Array(items) => {
            let values: Vec<String> = items
                .iter()
                .filter_map(|item| detail_value_to_string(item, name_by_id))
                .collect();
            (!values.is_empty()).then(|| values.join(", "))
        }
        Value::Object(map) => {
            if let Some(id) = map.get("id").and_then(Value::as_str) {
                let trimmed = id.trim();
                if looks_like_object_id(trimmed) {
                    return Some(format_object_ref(trimmed, name_by_id));
                }
            }
            for key in ["name", "key", "id", "value"] {
                if let Some(text) = map
                    .get(key)
                    .and_then(|value| detail_value_to_string(value, name_by_id))
                    && !text.is_empty()
                {
                    return Some(text);
                }
            }
            serde_json::to_string(map).ok()
        }
        Value::Null => None,
    }
}

fn format_object_ref(object_id: &str, name_by_id: &HashMap<String, String>) -> String {
    if let Some(name) = name_by_id.get(object_id)
        && !name.trim().is_empty()
    {
        return format!("{name} ({object_id})");
    }
    object_id.to_string()
}

fn looks_like_object_id(value: &str) -> bool {
    value.starts_with("baf") && value.chars().all(|c| c.is_ascii_alphanumeric())
}

fn value_as_rfc3339(value: &str) -> Option<String> {
    let text = value.trim();
    if text.is_empty() {
        return None;
    }
    if let Ok(parsed) = DateTime::parse_from_rfc3339(text) {
        return Some(
            parsed
                .with_timezone(&Utc)
                .to_rfc3339_opts(SecondsFormat::Secs, true),
        );
    }
    if let Ok(raw) = text.parse::<i64>() {
        return epoch_to_rfc3339(raw);
    }
    #[allow(clippy::cast_possible_truncation)]
    if let Ok(raw) = text.parse::<f64>() {
        return epoch_to_rfc3339(raw as i64);
    }
    None
}

fn epoch_to_rfc3339(raw: i64) -> Option<String> {
    let dt = if raw > 10_000_000_000 {
        DateTime::<Utc>::from_timestamp_millis(raw)
    } else {
        DateTime::<Utc>::from_timestamp(raw, 0)
    }?;
    Some(dt.to_rfc3339_opts(SecondsFormat::Secs, true))
}

fn yaml_quote(value: &str) -> String {
    let cleaned = value.replace(['\n', '\r'], " ").replace('\'', "''");
    format!("'{cleaned}'")
}

fn yaml_key(value: &str) -> String {
    if value
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-'))
    {
        value.to_string()
    } else {
        yaml_quote(value)
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

    #[test]
    fn input_mode_ctrl_shortcuts_edit_buffer() {
        let mut app = fixture_app();
        app.input_mode = InputMode::SaveAs;
        app.input_buffer = "alpha beta gamma".to_string();
        app.input_cursor = "alpha ".len();

        app.handle_action(KeyAction::KillToEnd);
        assert_eq!(app.input_buffer, "alpha ");

        app.input_buffer = "alpha beta".to_string();
        app.input_cursor = app.input_buffer.len();
        app.handle_action(KeyAction::CursorStart);
        assert_eq!(app.input_cursor, 0);

        app.handle_action(KeyAction::CursorEnd);
        assert_eq!(app.input_cursor, app.input_buffer.len());
    }

    #[test]
    fn half_page_scroll_uses_current_panel_viewport() {
        let mut app = fixture_app();
        app.focus = PanelFocus::Preview;
        app.preview_scroll_limit = (200, 20);
        app.handle_action(KeyAction::HalfPageDown);
        assert_eq!(app.preview_scroll, 10);
        app.handle_action(KeyAction::HalfPageUp);
        assert_eq!(app.preview_scroll, 0);

        app.focus = PanelFocus::Contents;
        app.contents_visible_rows = 10;
        app.table_state.select(Some(0));
        app.handle_action(KeyAction::HalfPageDown);
        assert_eq!(app.table_state.selected(), Some(2));
    }

    #[test]
    fn value_as_rfc3339_handles_epoch_seconds_and_millis() {
        assert_eq!(
            value_as_rfc3339("1700000000").as_deref(),
            Some("2023-11-14T22:13:20Z")
        );
        assert_eq!(
            value_as_rfc3339("1700000000000").as_deref(),
            Some("2023-11-14T22:13:20Z")
        );
    }

    #[test]
    fn yaml_front_matter_includes_expected_fields() {
        let details = serde_json::json!({
            "name": "Example",
            "spaceId": "space-123",
            "type": "note",
            "createdDate": 1_700_000_000,
            "lastModifiedDate": 1_700_003_600,
            "creator": "member-1"
        });
        let front = build_yaml_front_matter(
            "obj-1",
            "Fallback Name",
            "page",
            Some("fallback-space"),
            details.as_object().unwrap(),
            &HashMap::new(),
            &[],
        );

        assert!(front.contains("name: 'Example'"));
        assert!(front.contains("object_id: 'obj-1'"));
        assert!(front.contains("space_id: 'space-123'"));
        assert!(front.contains("type: 'note'"));
        assert!(front.contains("created_date: '2023-11-14T22:13:20Z'"));
        assert!(front.contains("last_modified_date: '2023-11-14T23:13:20Z'"));
        assert!(front.contains("creator: 'member-1'"));
    }

    #[test]
    fn yaml_front_matter_resolves_select_and_multi_select_ids() {
        let type_id = "bafyreitype111111111111111111111111111111111111111111111111";
        let tag_id = "bafyreitag1111111111111111111111111111111111111111111111111";
        let details = serde_json::json!({
            "name": "Example",
            "type": type_id,
            "spaceId": "space-1"
        });
        let names = HashMap::from([(type_id.to_string(), "Task".to_string())]);
        let props = vec![("Tags".to_string(), format!("Homework ({tag_id})"))];
        let front = build_yaml_front_matter(
            "obj-2",
            "Fallback Name",
            "page",
            Some("fallback-space"),
            details.as_object().unwrap(),
            &names,
            &props,
        );

        assert!(front.contains(&format!("type: 'Task ({type_id})'")));
        assert!(front.contains(&format!("Tags: 'Homework ({tag_id})'")));
        assert!(front.contains("properties:"));
    }
}
