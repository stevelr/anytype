use anytype::prelude::*;

pub trait TableRow {
    fn headers() -> &'static [&'static str];
    fn row(&self) -> Vec<String>;
}

pub fn render_table<T: TableRow>(items: &[T]) -> String {
    let headers = T::headers();
    let rows: Vec<Vec<String>> = items.iter().map(|item| item.row()).collect();
    let widths = column_widths(headers, &rows);

    let mut out = String::new();
    out.push_str(&format_row(
        &headers.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
        &widths,
    ));
    out.push('\n');
    out.push_str(&format_separator(&widths));

    for row in rows {
        out.push('\n');
        out.push_str(&format_row(&row, &widths));
    }

    out
}

fn column_widths(headers: &[&str], rows: &[Vec<String>]) -> Vec<usize> {
    let mut widths: Vec<usize> = headers.iter().map(|h| h.len()).collect();
    for row in rows {
        for (idx, cell) in row.iter().enumerate() {
            if idx >= widths.len() {
                widths.push(cell.len());
            } else {
                widths[idx] = widths[idx].max(cell.len());
            }
        }
    }
    widths
}

fn format_row(row: &[String], widths: &[usize]) -> String {
    let mut out = String::new();
    for (idx, cell) in row.iter().enumerate() {
        if idx > 0 {
            out.push_str("  ");
        }
        let width = widths.get(idx).copied().unwrap_or(0);
        out.push_str(&format!("{cell:<width$}"));
    }
    out
}

fn format_separator(widths: &[usize]) -> String {
    let mut out = String::new();
    for (idx, width) in widths.iter().enumerate() {
        if idx > 0 {
            out.push_str("  ");
        }
        out.push_str(&"-".repeat(*width));
    }
    out
}

impl TableRow for Space {
    fn headers() -> &'static [&'static str] {
        &["id", "name", "model"]
    }

    fn row(&self) -> Vec<String> {
        vec![self.id.clone(), self.name.clone(), self.object.to_string()]
    }
}

impl TableRow for Object {
    fn headers() -> &'static [&'static str] {
        &["id", "name", "type", "archived"]
    }

    fn row(&self) -> Vec<String> {
        let name = self.name.clone().unwrap_or_default();
        let type_key = self
            .r#type
            .as_ref()
            .map(|t| t.key.clone())
            .unwrap_or_default();
        vec![self.id.clone(), name, type_key, self.archived.to_string()]
    }
}

impl TableRow for Type {
    fn headers() -> &'static [&'static str] {
        &["id", "key", "name", "layout"]
    }

    fn row(&self) -> Vec<String> {
        let name = self.name.clone().unwrap_or_default();
        vec![
            self.id.clone(),
            self.key.clone(),
            name,
            self.layout.to_string(),
        ]
    }
}

impl TableRow for Property {
    fn headers() -> &'static [&'static str] {
        &["id", "key", "name", "format"]
    }

    fn row(&self) -> Vec<String> {
        vec![
            self.id.clone(),
            self.key.clone(),
            self.name.clone(),
            self.format().to_string(),
        ]
    }
}

impl TableRow for Member {
    fn headers() -> &'static [&'static str] {
        &["id", "name", "role", "status"]
    }

    fn row(&self) -> Vec<String> {
        vec![
            self.id.clone(),
            self.display_name().to_string(),
            self.role.to_string(),
            self.status.to_string(),
        ]
    }
}

impl TableRow for Tag {
    fn headers() -> &'static [&'static str] {
        &["id", "key", "name", "color"]
    }

    fn row(&self) -> Vec<String> {
        vec![
            self.id.clone(),
            self.key.clone(),
            self.name.clone(),
            self.color.to_string(),
        ]
    }
}

impl TableRow for View {
    fn headers() -> &'static [&'static str] {
        &["id", "name", "layout", "sorts", "filters"]
    }

    fn row(&self) -> Vec<String> {
        let layout = self.layout.to_string();
        let name = self.name.clone().unwrap_or_default();
        vec![
            self.id.clone(),
            name,
            layout,
            self.sorts.len().to_string(),
            self.filters.len().to_string(),
        ]
    }
}
