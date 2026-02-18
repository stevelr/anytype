mod app;
mod index;
mod keys;
mod ui;

use std::path::Path;

use anyhow::Result;

pub fn run_inspector(path: &Path, max_cache_bytes: usize) -> Result<()> {
    app::App::run(path, max_cache_bytes)
}
