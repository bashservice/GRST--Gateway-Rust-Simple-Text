use base64::{engine::general_purpose::STANDARD, Engine as _};
use image::{imageops::FilterType, GrayImage};

pub const MAX_IMG_BYTES: u64 = 1024 * 1024; // 1 MB

const CONTRAST: f32 = 1.45;
const GAMMA: f32 = 1.25;
const EDGE_STRENGTH: f32 = 0.35;

// Bity kropek Braille'a w układzie 2 kolumny × 4 rzędy:
// [0, 3]
// [1, 4]
// [2, 5]
// [6, 7]
const BRAILLE_BITS: [[u32; 2]; 4] = [
    [0, 3],
    [1, 4],
    [2, 5],
    [6, 7],
];

pub fn estimate_data_uri_bytes(url: &str) -> Option<u64> {
    if !url.starts_with("data:") {
        return None;
    }

    let (meta, payload) = url.split_once(',')?;

    if meta.contains(";base64") {
        let clean_len = payload
            .bytes()
            .filter(|byte| !byte.is_ascii_whitespace())
            .count() as u64;

        Some(clean_len.saturating_mul(3).saturating_add(3) / 4)
    } else {
        Some(payload.len() as u64)
    }
}

pub fn format_display_url(url: &str) -> String {
    if url.starts_with("data:") {
        if let Some((meta, _)) = url.split_once(',') {
            let size = estimate_data_uri_bytes(url)
                .map(|bytes| format!("{:.1} KB", bytes as f64 / 1024.0))
                .unwrap_or_else(|| "? KB".to_string());

            return [meta, ",<image: ", &size, ">"].concat();
        }
    }

    url.to_string()
}

fn decode_image_bytes(url: &str, raw_bytes: &[u8]) -> Option<Vec<u8>> {
    if !raw_bytes.is_empty() {
        return Some(raw_bytes.to_vec());
    }

    if !url.starts_with("data:") {
        return None;
    }

    let (meta, payload) = url.split_once(',')?;

    if !meta.contains(";base64") {
        return None;
    }

    let clean_payload: String = payload
        .chars()
        .filter(|character| !character.is_ascii_whitespace())
        .collect();

    STANDARD.decode(clean_payload).ok()
}

fn sharpen_luma(gray: &GrayImage) -> Vec<f32> {
    let w = gray.width() as usize;
    let h = gray.height() as usize;
    let mut buf = vec![0.0f32; w * h];

    for y in 0..h {
        for x in 0..w {
            let c = gray.get_pixel(x as u32, y as u32)[0] as f32;
            let contrasted = ((c - 128.0) * CONTRAST + 128.0).clamp(0.0, 255.0);
            buf[y * w + x] = (contrasted / 255.0).powf(GAMMA) * 255.0;
        }
    }

    if EDGE_STRENGTH <= 0.0 || w < 3 || h < 3 {
        return buf;
    }

    let mut out = buf.clone();
    for y in 1..(h - 1) {
        for x in 1..(w - 1) {
            let center = buf[y * w + x];
            let avg_neighbors = (buf[(y - 1) * w + x]
                + buf[(y + 1) * w + x]
                + buf[y * w + (x - 1)]
                + buf[y * w + (x + 1)])
                * 0.25;
            let sharpened = center + (center - avg_neighbors) * EDGE_STRENGTH;
            out[y * w + x] = sharpened.clamp(0.0, 255.0);
        }
    }

    out
}

/// Tworzy monochromatyczną wersję obrazu w postaci tekstu Braille'a z ditheringiem Floyda-Steinberga.
pub fn render_retro_bw_image(
    label: &str,
    url: &str,
    size_bytes: Option<u64>,
    raw_bytes: &[u8],
    width: usize,
    height: usize,
) -> Option<(String, Vec<String>)> {
    if size_bytes.is_some_and(|size| size > MAX_IMG_BYTES) {
        return None;
    }

    let image_bytes = decode_image_bytes(url, raw_bytes)?;

    if image_bytes.is_empty() || image_bytes.len() as u64 > MAX_IMG_BYTES {
        return None;
    }

    let image = image::load_from_memory(&image_bytes).ok()?;

    let output_width = width.clamp(16, 100) as u32;
    let max_height = height.clamp(6, 48) as u32;

    let source_width = image.width();
    let source_height = image.height();

    if source_width == 0 || source_height == 0 {
        return None;
    }

    let natural_height =
        (output_width as f32 * source_height as f32 / source_width as f32 * 0.5).round() as u32;

    let output_height = natural_height.clamp(6, max_height);

    let pixel_width = (output_width * 2) as usize;
    let pixel_height = (output_height * 4) as usize;

    let gray: GrayImage = image
        .resize_exact(pixel_width as u32, pixel_height as u32, FilterType::Lanczos3)
        .to_luma8();

    let mut work = sharpen_luma(&gray);

    // Floyd-Steinberg: zapalamy kropki dla ciemniejszych partii (< 128)
    for y in 0..pixel_height {
        for x in 0..pixel_width {
            let idx = y * pixel_width + x;
            let old_val = work[idx];
            let new_val = if old_val < 128.0 { 0.0 } else { 255.0 };
            let err = old_val - new_val;
            work[idx] = new_val;

            if x + 1 < pixel_width {
                work[y * pixel_width + (x + 1)] =
                    (work[y * pixel_width + (x + 1)] + err * (7.0 / 16.0)).clamp(0.0, 255.0);
            }
            if y + 1 < pixel_height {
                if x > 0 {
                    work[(y + 1) * pixel_width + (x - 1)] =
                        (work[(y + 1) * pixel_width + (x - 1)] + err * (3.0 / 16.0))
                            .clamp(0.0, 255.0);
                }
                work[(y + 1) * pixel_width + x] =
                    (work[(y + 1) * pixel_width + x] + err * (5.0 / 16.0)).clamp(0.0, 255.0);
                if x + 1 < pixel_width {
                    work[(y + 1) * pixel_width + (x + 1)] =
                        (work[(y + 1) * pixel_width + (x + 1)] + err * (1.0 / 16.0))
                            .clamp(0.0, 255.0);
                }
            }
        }
    }

    let mut lines = Vec::with_capacity(output_height as usize);

    for cell_y in 0..output_height as usize {
        let mut row = String::with_capacity(output_width as usize * 3);

        for cell_x in 0..output_width as usize {
            let mut dots = 0u32;

            for dy in 0..4usize {
                for dx in 0..2usize {
                    let x = cell_x * 2 + dx;
                    let y = cell_y * 4 + dy;
                    if work[y * pixel_width + x] < 128.0 {
                        dots |= 1 << BRAILLE_BITS[dy][dx];
                    }
                }
            }

            if dots == 0 {
                row.push(' ');
            } else if let Some(ch) = char::from_u32(0x2800 + dots) {
                row.push(ch);
            }
        }

        lines.push(row);
    }

    let actual_size = size_bytes.unwrap_or(image_bytes.len() as u64);
    let size_text = format!("{:.1} KB", actual_size as f64 / 1024.0);

    let safe_label = label
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;");

    let description = [
        "1980 B&W Braille - ",
        safe_label.as_str(),
        " - ",
        size_text.as_str(),
    ]
    .concat();

    Some((description, lines))
}