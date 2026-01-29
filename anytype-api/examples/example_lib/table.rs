#[allow(dead_code)]
pub fn render_table(headers: &[&str], rows: &[Vec<String>]) -> String {
    let mut widths: Vec<usize> = headers.iter().map(|header| header.len()).collect();
    for row in rows {
        for (idx, cell) in row.iter().enumerate() {
            if idx >= widths.len() {
                widths.push(cell.len());
            } else {
                widths[idx] = widths[idx].max(cell.len());
            }
        }
    }

    let mut out = String::new();
    out.push_str(&format_row(
        &headers
            .iter()
            .map(|value| value.to_string())
            .collect::<Vec<_>>(),
        &widths,
    ));
    out.push('\n');
    out.push_str(&format_separator(&widths));
    for row in rows {
        out.push('\n');
        out.push_str(&format_row(row, &widths));
    }
    out
}

#[allow(dead_code)]
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

#[allow(dead_code)]
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
