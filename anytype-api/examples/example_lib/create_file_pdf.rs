use markdown2pdf::{config::ConfigSource, parse_into_file};
use std::path::{Path, PathBuf};

// Generate PDF file from the project README.md
// Returns the path to the generated file
#[allow(dead_code)]
pub fn create_pdf(dir: &Path) -> Result<PathBuf, anyhow::Error> {
    let pdf_path = dir.join("my_pdf.pdf");

    let readme_md =
        std::fs::read_to_string(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("README.md"))?;

    parse_into_file(
        readme_md,
        &pdf_path.to_string_lossy(),
        ConfigSource::Default,
        Default::default(),
    )?;

    println!("wrote pdf: {}", pdf_path.display());
    Ok(pdf_path)
}
