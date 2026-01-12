use anyhow::Result;
use serde::Serialize;
use std::fs;
use std::path::PathBuf;

mod table;

pub use table::{TableRow, render_table, render_table_dynamic};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OutputFormat {
    Json,
    Pretty,
    Table,
    Quiet,
}

#[derive(Clone, Debug)]
pub struct Output {
    format: OutputFormat,
    path: Option<PathBuf>,
}

impl Output {
    pub fn new(format: OutputFormat, path: Option<PathBuf>) -> Self {
        Self { format, path }
    }

    pub fn format(&self) -> OutputFormat {
        self.format
    }

    pub fn emit_json<T: Serialize + ?Sized>(&self, value: &T) -> Result<()> {
        if self.format == OutputFormat::Quiet {
            return Ok(());
        }

        let data = match self.format {
            OutputFormat::Pretty => serde_json::to_string_pretty(value)?,
            _ => serde_json::to_string(value)?,
        };

        self.write(&data)
    }

    pub fn emit_table<T: TableRow + Serialize + Sized>(&self, items: &[T]) -> Result<()> {
        match self.format {
            OutputFormat::Table => {
                let data = render_table(items);
                self.write(&data)
            }
            OutputFormat::Quiet => Ok(()),
            _ => self.emit_json(items),
        }
    }

    pub fn emit_text(&self, text: &str) -> Result<()> {
        if self.format == OutputFormat::Quiet {
            return Ok(());
        }
        self.write(text)
    }

    fn write(&self, data: &str) -> Result<()> {
        let mut output = data.to_string();
        if !output.ends_with('\n') {
            output.push('\n');
        }

        if let Some(path) = &self.path {
            fs::write(path, output)?;
        } else {
            print!("{output}");
        }
        Ok(())
    }
}
