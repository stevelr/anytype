use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Cell, Clear, Paragraph, Row, Table, Wrap},
};
use ratatui_image::protocol::StatefulProtocol;
use ratatui_image::{FilterType, Resize, StatefulImage};

use super::app::{App, InputMode, PanelFocus};
use super::index::SortColumn;

/// Count the total rendered lines a `Text` will occupy when wrapped to `width` columns.
#[allow(clippy::cast_possible_truncation)]
fn wrapped_line_count(text: &Text, width: u16) -> u16 {
    if width == 0 {
        return 0;
    }
    let w = width as usize;
    text.lines
        .iter()
        .map(|line| {
            let line_width = line.width();
            if line_width == 0 {
                1 // empty lines still occupy one row
            } else {
                line_width.div_ceil(w)
            }
        })
        .sum::<usize>() as u16
}

const HELP_TEXT: &[(&str, &str)] = &[
    ("j / Down", "Move down"),
    ("k / Up", "Move up"),
    ("PgDn", "Page down"),
    ("PgUp", "Page up"),
    ("g / Home", "Jump to first"),
    ("G / End", "Jump to last"),
    ("Tab / ]", "Next panel"),
    ("BackTab / [", "Prev panel"),
    ("1..5", "Jump to panel"),
    ("/", "Search by title"),
    ("f", "Filter by type"),
    ("w", "Save selected object"),
    ("Ctrl-e", "Open selected object in $EDITOR"),
    ("Ctrl-o", "Open selected object in Anytype"),
    ("Ctrl-c", "Copy selected object id"),
    ("Ctrl-d / Ctrl-u", "Half-page down / up"),
    ("Ctrl-a/e/k", "In input: start/end/kill-to-eol"),
    ("Esc", "Clear filters / dismiss"),
    ("Enter", "Apply input / follow link"),
    ("b", "Back to previous object"),
    ("s / S", "Sort / reverse"),
    ("?", "Toggle help"),
    ("q", "Quit"),
];

pub fn draw(frame: &mut Frame, app: &mut App) {
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(8),
            Constraint::Length(1),
        ])
        .split(frame.area());

    draw_status_bar(frame, app, outer[0]);
    draw_main_area(frame, app, outer[1]);
    draw_footer(frame, app, outer[2]);

    if app.show_help {
        draw_help_overlay(frame, frame.area());
    }
}

fn draw_status_bar(frame: &mut Frame, app: &App, area: Rect) {
    let archive_name = std::path::Path::new(&app.index.archive_path)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(&app.index.archive_path);
    let mut filter_parts = Vec::new();
    if !app.search_query.is_empty() {
        filter_parts.push(format!("search=\"{}\"", app.search_query));
    }
    if let Some(t) = &app.type_filter {
        filter_parts.push(format!("type=\"{t}\""));
    }
    let filters = if filter_parts.is_empty() {
        "no filters".to_string()
    } else {
        filter_parts.join(" | ")
    };
    let status = format!(
        " {} | {} | {}/{} objects | {} | {} | {}",
        archive_name,
        app.index.source_kind.as_str(),
        app.filtered_indices.len(),
        app.index.entries.len(),
        app.index.format,
        app.index.created_at,
        filters,
    );
    let bar = Paragraph::new(status).style(
        Style::default()
            .bg(Color::DarkGray)
            .fg(Color::White)
            .add_modifier(Modifier::BOLD),
    );
    frame.render_widget(bar, area);
}

fn draw_main_area(frame: &mut Frame, app: &mut App, area: Rect) {
    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(44), Constraint::Percentage(56)])
        .split(area);

    let left_links_height = ((columns[0].height.saturating_mul(20)) / 100).clamp(3, 8);
    let left = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(columns[0].height.saturating_sub(left_links_height)),
            Constraint::Length(left_links_height),
        ])
        .split(columns[0]);

    draw_contents_panel(frame, app, left[0]);
    draw_links_panel(frame, app, left[1]);

    let right = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage(50),
            Constraint::Percentage(25),
            Constraint::Percentage(25),
        ])
        .split(columns[1]);

    draw_preview_panel(frame, app, right[0]);
    draw_properties_panel(frame, app, right[1]);
    draw_metadata_panel(frame, app, right[2]);
}

fn draw_contents_panel(frame: &mut Frame, app: &mut App, area: Rect) {
    let focused = app.focus == PanelFocus::Contents;
    let border_style = if focused {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    let sort_indicator = |col: SortColumn| -> &'static str {
        if col == app.sort.column {
            if app.sort.ascending { " ^" } else { " v" }
        } else {
            ""
        }
    };

    let header_cells = [
        Cell::from(format!("Id{}", sort_indicator(SortColumn::Id))),
        Cell::from(format!("Type{}", sort_indicator(SortColumn::Type))),
        Cell::from(format!("Name{}", sort_indicator(SortColumn::Name))),
    ];
    let header = Row::new(header_cells)
        .style(
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )
        .height(1);

    let rows: Vec<Row> = app
        .filtered_indices
        .iter()
        .filter_map(|&idx| app.index.entries.get(idx))
        .map(|entry| {
            Row::new([
                Cell::from(entry.short_id.as_str()),
                Cell::from(entry.type_display.as_str()),
                Cell::from(entry.name.as_str()),
            ])
        })
        .collect();
    app.contents_visible_rows = area.height.saturating_sub(3).max(1);

    let widths = [
        Constraint::Length(7),
        Constraint::Length(14),
        Constraint::Min(16),
    ];

    let title = if app.filtered_indices.len() == app.index.entries.len() {
        format!(" Contents ({}) ", app.index.entries.len())
    } else {
        format!(
            " Search Results ({}/{}) ",
            app.filtered_indices.len(),
            app.index.entries.len()
        )
    };

    let table = Table::new(rows, widths)
        .header(header)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(title)
                .border_style(border_style),
        )
        .row_highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("> ");

    frame.render_stateful_widget(table, area, &mut app.table_state);
}

fn draw_preview_panel(frame: &mut Frame, app: &mut App, area: Rect) {
    let focused = app.focus == PanelFocus::Preview;
    let border_style = if focused {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::DarkGray)
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Preview ")
        .border_style(border_style);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    app.prepare_image_preview();
    app.prepare_markdown_preview();
    if let Some(state) = app.image_preview.as_mut() {
        let image = StatefulImage::<StatefulProtocol>::default()
            .resize(Resize::Fit(Some(FilterType::CatmullRom)));
        frame.render_stateful_widget(image, inner, state);
        return;
    }

    let text = app.current_entry().map_or_else(
        || Text::from("No object selected"),
        |entry| {
            if let Some(error) = app.image_preview_error.as_deref() {
                Text::from(vec![
                    Line::from(Span::styled(
                        "Image preview",
                        Style::default()
                            .fg(Color::Yellow)
                            .add_modifier(Modifier::BOLD),
                    )),
                    Line::from(""),
                    Line::from(error.to_string()),
                    Line::from(""),
                    Line::from("Text preview"),
                    Line::from(""),
                    Line::from(
                        app.markdown_preview
                            .clone()
                            .unwrap_or_else(|| entry.preview.clone()),
                    ),
                ])
            } else if let Some(markdown) = app.markdown_preview.as_deref() {
                styled_markdown_preview(markdown)
            } else if let Some(error) = app.markdown_preview_error.as_deref() {
                Text::from(vec![
                    Line::from(Span::styled(
                        "Markdown preview",
                        Style::default()
                            .fg(Color::Yellow)
                            .add_modifier(Modifier::BOLD),
                    )),
                    Line::from(""),
                    Line::from(error.to_string()),
                    Line::from(""),
                    Line::from("Fallback text preview"),
                    Line::from(""),
                    Line::from(entry.preview.clone()),
                ])
            } else {
                styled_markdown_preview(&entry.preview)
            }
        },
    );

    app.preview_scroll_limit = (wrapped_line_count(&text, inner.width), inner.height);

    let paragraph = Paragraph::new(text)
        .scroll((app.preview_scroll, 0))
        .wrap(Wrap { trim: false });

    frame.render_widget(paragraph, inner);
}

fn draw_properties_panel(frame: &mut Frame, app: &mut App, area: Rect) {
    let focused = app.focus == PanelFocus::Properties;
    let border_style = if focused {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::DarkGray)
    };

    let lines: Vec<Line> = app.current_entry().map_or_else(
        || vec![Line::from("  No object selected")],
        |entry| {
            if entry.properties.is_empty() {
                vec![Line::from("  (no user properties)")]
            } else {
                entry
                    .properties
                    .iter()
                    .map(|(key, value)| {
                        let mut spans = vec![Span::styled(
                            format!("  {key}: "),
                            Style::default()
                                .fg(Color::Yellow)
                                .add_modifier(Modifier::BOLD),
                        )];
                        spans.extend(property_value_spans(value));
                        Line::from(spans)
                    })
                    .collect()
            }
        },
    );

    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Properties ")
        .border_style(border_style);
    let inner = block.inner(area);
    let text = Text::from(lines);
    app.properties_scroll_limit = (wrapped_line_count(&text, inner.width), inner.height);

    let paragraph = Paragraph::new(text)
        .block(block)
        .wrap(Wrap { trim: false })
        .scroll((app.properties_scroll, 0));

    frame.render_widget(paragraph, area);
}

fn draw_metadata_panel(frame: &mut Frame, app: &mut App, area: Rect) {
    let focused = app.focus == PanelFocus::Metadata;
    let border_style = if focused {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::DarkGray)
    };

    let selected = app.current_entry();

    let lines: Vec<Line> = selected.map_or_else(
        || vec![Line::from("  No object selected")],
        |entry| {
            let mut lines = vec![
                meta_line("Object Id", &entry.id),
                meta_line("Type", &entry.type_display),
                meta_line("Name", &entry.name),
                meta_line("Created", &entry.created),
                meta_line("Modified", &entry.modified),
                meta_line("Size", &entry.size_display),
                meta_line("Layout", &entry.layout_name),
                meta_line("SB Type", &entry.sb_type),
                meta_line("Archived", if entry.archived { "yes" } else { "no" }),
                meta_line("Path", &entry.path),
                meta_line("Readable", if entry.readable { "yes" } else { "no" }),
            ];
            if !entry.type_id.is_empty() {
                lines.push(meta_line("Type Id", &entry.type_id));
            }
            if let Some(type_name) = &entry.type_name {
                lines.push(meta_line("Type Name", type_name));
            }
            lines.push(
                entry
                    .error
                    .as_ref()
                    .map_or_else(|| Line::from(""), |err| meta_line("Error", err)),
            );
            lines
        },
    );

    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Metadata ")
        .border_style(border_style);
    let inner = block.inner(area);
    let text = Text::from(lines);
    app.meta_scroll_limit = (wrapped_line_count(&text, inner.width), inner.height);

    let paragraph = Paragraph::new(text)
        .block(block)
        .wrap(Wrap { trim: false })
        .scroll((app.meta_scroll, 0));

    frame.render_widget(paragraph, area);
}

fn draw_links_panel(frame: &mut Frame, app: &mut App, area: Rect) {
    let focused = app.focus == PanelFocus::Links;
    let border_style = if focused {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::DarkGray)
    };

    let links = app.current_links();

    let header = Row::new(["Rel", "Target", "Name"]).style(
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
    );

    let rows: Vec<Row> = links
        .iter()
        .map(|row| {
            let short = if row.target_id.len() >= 5 {
                row.target_id[row.target_id.len() - 5..].to_string()
            } else {
                row.target_id.clone()
            };
            Row::new([
                Cell::from(row.relation),
                Cell::from(short),
                Cell::from(row.target_name.as_str()),
            ])
        })
        .collect();
    app.links_visible_rows = area.height.saturating_sub(3).max(1);

    let table = Table::new(
        rows,
        [
            Constraint::Length(9),
            Constraint::Length(8),
            Constraint::Min(16),
        ],
    )
    .header(header)
    .block(
        Block::default()
            .borders(Borders::ALL)
            .title(" Links & Backlinks ")
            .border_style(border_style),
    )
    .row_highlight_style(
        Style::default()
            .bg(Color::DarkGray)
            .add_modifier(Modifier::BOLD),
    )
    .highlight_symbol("> ");

    frame.render_stateful_widget(table, area, &mut app.link_state);
}

fn meta_line(label: &str, value: &str) -> Line<'static> {
    Line::from(vec![
        Span::styled(
            format!("  {label:<12}"),
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(value.to_string()),
    ])
}

fn styled_markdown_preview(preview: &str) -> Text<'static> {
    let lines = preview
        .lines()
        .map(|line| {
            let style = markdown_line_style(line);
            Line::from(Span::styled(line.to_string(), style))
        })
        .collect::<Vec<_>>();
    Text::from(lines)
}

fn markdown_line_style(line: &str) -> Style {
    let trimmed = line.trim_start();
    if trimmed.starts_with("### ") {
        return Style::default()
            .fg(Color::LightGreen)
            .add_modifier(Modifier::BOLD);
    }
    if trimmed.starts_with("## ") {
        return Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD);
    }
    if trimmed.starts_with("# ") {
        return Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD);
    }
    Style::default()
}

fn property_value_spans(value: &str) -> Vec<Span<'static>> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut cursor = 0usize;

    while cursor < value.len() {
        let Some(rel_open) = value[cursor..].find('(') else {
            break;
        };
        let open = cursor + rel_open;
        let Some(rel_close) = value[open..].find(')') else {
            break;
        };
        let close = open + rel_close;
        let id_candidate = value[open + 1..close].trim();
        if looks_like_object_id(id_candidate) {
            if open > cursor {
                spans.push(Span::raw(value[cursor..open].to_string()));
            }
            spans.push(Span::styled(
                value[open..=close].to_string(),
                Style::default().fg(Color::DarkGray),
            ));
            cursor = close + 1;
        } else {
            cursor = open + 1;
        }
    }

    if cursor < value.len() {
        spans.push(Span::raw(value[cursor..].to_string()));
    }

    if spans.is_empty() {
        spans.push(Span::raw(value.to_string()));
    }
    spans
}

fn looks_like_object_id(value: &str) -> bool {
    value.starts_with("baf") && value.chars().all(|c| c.is_ascii_alphanumeric())
}

fn draw_footer(frame: &mut Frame, app: &App, area: Rect) {
    let (text, prefix_len) = match app.input_mode {
        InputMode::Search => {
            let prefix = "Search title: ";
            (format!("{prefix}{}", app.input_buffer), prefix.len())
        }
        InputMode::Filter => {
            let prefix = "Type filter: ";
            (format!("{prefix}{}", app.input_buffer), prefix.len())
        }
        InputMode::SaveAs => {
            let prefix = "Save as path: ";
            (format!("{prefix}{}", app.input_buffer), prefix.len())
        }
        InputMode::None => {
            let t = app.status_message.as_ref().map_or_else(|| " j/k:move  Ctrl-d/u:half-page  Tab:panel  /:search  f:filter  w:save-as  Ctrl-e:editor  Ctrl-o:anytype  Ctrl-c:copy-id  Enter:follow  b:back  s/S:sort  ?:help  q:quit"
                    .to_string(), std::clone::Clone::clone);
            (t, 0)
        }
    };
    let style = if app.input_mode == InputMode::None {
        Style::default().fg(Color::DarkGray)
    } else {
        Style::default().fg(Color::Yellow)
    };
    frame.render_widget(Paragraph::new(text).style(style), area);

    if app.input_mode != InputMode::None {
        // Count display columns up to cursor byte offset
        let chars_before_cursor = app.input_buffer[..app.input_cursor].chars().count();
        #[allow(clippy::cast_possible_truncation)]
        let cursor_x = area.x + (prefix_len + chars_before_cursor) as u16;
        let cursor_y = area.y;
        if cursor_x < area.x + area.width {
            frame.set_cursor_position((cursor_x, cursor_y));
        }
    }
}

#[allow(clippy::cast_possible_truncation)]
fn draw_help_overlay(frame: &mut Frame, area: Rect) {
    let width = 56u16.min(area.width.saturating_sub(4));
    let height = (HELP_TEXT.len() as u16 + 4).min(area.height.saturating_sub(2));
    let x = area.x + (area.width.saturating_sub(width)) / 2;
    let y = area.y + (area.height.saturating_sub(height)) / 2;
    let popup_area = Rect::new(x, y, width, height);

    frame.render_widget(Clear, popup_area);

    let lines: Vec<Line> = HELP_TEXT
        .iter()
        .map(|(key, desc)| {
            Line::from(vec![
                Span::styled(
                    format!("  {key:<18}"),
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw(*desc),
            ])
        })
        .collect();

    let help = Paragraph::new(lines).block(
        Block::default()
            .borders(Borders::ALL)
            .title(" Keybindings ")
            .border_style(Style::default().fg(Color::Cyan)),
    );
    frame.render_widget(help, popup_area);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::inspector::app::App;
    use crate::cli::inspector::index::ArchiveIndex;
    use anyback_reader::archive::ArchiveSourceKind;
    use ratatui::{Terminal, backend::TestBackend};

    fn fixture_index() -> ArchiveIndex {
        use crate::cli::inspector::index::ObjectEntry;
        let id_a = "bafyreiaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let id_b = "bafyreibbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

        let entry = |id: &str, name: &str, links: Vec<String>, backlinks: Vec<String>| {
            let short_id = id[id.len() - 5..].to_string();
            ObjectEntry {
                id: id.to_string(),
                short_id,
                type_id: String::new(),
                type_short_id: String::new(),
                type_name: Some("page".to_string()),
                type_display: "page".to_string(),
                name: name.to_string(),
                sb_type: "TEXT".to_string(),
                layout_name: "page".to_string(),
                created: "-".to_string(),
                modified: "-".to_string(),
                created_epoch: 0,
                modified_epoch: 0,
                size: 42,
                size_display: "42 B".to_string(),
                archived: false,
                path: format!("objects/{id}.pb"),
                readable: true,
                error: None,
                preview: format!("preview {name}\nsecond line"),
                file_mime: None,
                image_payload_path: None,
                properties_count: 0,
                properties: Vec::new(),
                links,
                backlinks,
            }
        };

        ArchiveIndex {
            archive_path: "fixture.abk".to_string(),
            source_kind: ArchiveSourceKind::Directory,
            manifest: None,
            manifest_error: None,
            file_count: 2,
            total_bytes: 84,
            format: "pb-json".to_string(),
            created_at: "2026-02-13 00:00:00 UTC".to_string(),
            entries: vec![
                entry(id_a, "Alpha", vec![id_b.to_string()], Vec::new()),
                entry(id_b, "Beta", Vec::new(), vec![id_a.to_string()]),
            ],
        }
    }

    fn buffer_to_lines(term: &Terminal<TestBackend>) -> Vec<String> {
        let buf = term.backend().buffer();
        let area = buf.area();
        (0..area.height)
            .map(|y| {
                (0..area.width)
                    .map(|x| buf[(x, y)].symbol())
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect()
    }

    #[test]
    fn snapshot_default_layout_contains_core_panels() {
        let backend = TestBackend::new(100, 30);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut app = App::new(fixture_index());

        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        let lines = buffer_to_lines(&terminal);
        let screen = lines.join("\n");

        assert!(screen.contains("Contents"));
        assert!(screen.contains("Preview"));
        assert!(screen.contains("Properties"));
        assert!(screen.contains("Metadata"));
        assert!(screen.contains("Links & Backlinks"));
        assert!(screen.contains("search"));
    }

    #[test]
    fn snapshot_search_prompt_and_help_overlay_render() {
        let backend = TestBackend::new(100, 30);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut app = App::new(fixture_index());
        app.input_mode = InputMode::Search;
        app.input_buffer = "alpha".to_string();
        app.show_help = true;

        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        let lines = buffer_to_lines(&terminal);
        let screen = lines.join("\n");

        assert!(screen.contains("Search title: alpha"));
        assert!(screen.contains("Keybindings"));
        assert!(screen.contains("Search by title"));
    }

    #[test]
    fn property_value_spans_downtones_object_id_suffix() {
        let id = "bafyreixxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx";
        let spans = property_value_spans(&format!("Open ({id}), Next"));
        let rendered: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(rendered, format!("Open ({id}), Next"));
        assert!(spans.iter().any(
            |s| s.content.as_ref() == format!("({id})") && s.style.fg == Some(Color::DarkGray)
        ));
    }
}
