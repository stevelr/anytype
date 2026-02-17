mod app;
mod index;
mod keys;
mod ui;

use std::path::Path;

use anyhow::Result;

pub fn run_inspector(path: &Path) -> Result<()> {
    app::App::run(path)
}
