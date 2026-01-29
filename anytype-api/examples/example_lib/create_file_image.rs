// Generating PNG images

use anyhow::{Context, Result};
use std::{
    fs::File,
    io::BufWriter,
    path::{Path, PathBuf},
};

use tiny_skia::{Color, Paint, Pixmap, Rect, Transform};

#[allow(dead_code)]
const COLORS: &[(f32, f32, f32, f32)] = &[
    (1.0, 0.443, 0.808, 1.0),   // (255, 113, 206)
    (0.004, 0.804, 0.996, 1.0), // (1, 205, 254)
    (0.020, 1.0, 0.631, 1.0),   // (5, 255, 161)
    (0.725, 0.404, 1.0, 1.0),   // (185, 103, 255)
    (1.0, 0.984, 0.588, 1.0),   // (255, 251, 150)
];

/// Create a png image - a square with solid fill
/// # Parameters
/// * size: square width and height
/// * color_num: one of the preset colors (0-4 inclusive)
/// * temp_dir: folder in which to create the file
///
#[allow(dead_code)]
pub fn create_png(size: u32, color_num: usize, temp_dir: &Path) -> Result<PathBuf> {
    let solid_path = temp_dir.join(format!("solid_square_{color_num}_{size}.png"));
    create_solid_rectangle(size, size, COLORS[color_num], &solid_path)?;
    println!("Created: {}", solid_path.display());
    Ok(solid_path)
}

/// Creates a PNG file with a solid colored rectangle.
///
/// # Arguments
/// * `width` - Width of the image in pixels
/// * `height` - Height of the image in pixels
/// * `color` - RGBA color (each component 0.0-1.0)
/// * `output_path` - Path where the PNG file will be saved
#[allow(dead_code)]
pub fn create_solid_rectangle(
    width: u32,
    height: u32,
    color: (f32, f32, f32, f32),
    output_path: &Path,
) -> Result<()> {
    let mut pixmap = Pixmap::new(width, height).context("Failed to create pixmap")?;

    let rect = Rect::from_xywh(0.0, 0.0, width as f32, height as f32).context("rectangle")?;

    let mut paint = Paint::default();
    paint.set_color(Color::from_rgba(color.0, color.1, color.2, color.3).context("invalid color")?);

    pixmap.fill_rect(rect, &paint, Transform::identity(), None);

    save_pixmap_as_png(&pixmap, output_path)?;

    Ok(())
}

/// Saves a Pixmap to a PNG file.
#[allow(dead_code)]
fn save_pixmap_as_png(pixmap: &Pixmap, path: &Path) -> Result<()> {
    let file = File::create(path).context("create image file")?;
    let writer = BufWriter::new(file);

    let mut encoder = png::Encoder::new(writer, pixmap.width(), pixmap.height());
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);

    let mut writer = encoder.write_header()?;
    writer.write_image_data(pixmap.data())?;

    Ok(())
}
