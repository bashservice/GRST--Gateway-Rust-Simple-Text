use base64::{engine::general_purpose::STANDARD, Engine as _};
use image::{imageops::FilterType, GrayImage};

pub const MAX_IMG_BYTES: u64 = 1024 * 1024; // 1 MB


const BW_PIXELS: [char; 5] = [' ', '.', ':', ';', '*'];


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

        Some(clean_len.saturating_mul(3) / 4)
    } else {
        Some(payload.len() as u64)
    }
}

pub fn format_display_url(url: &str) -> String {
    if let Some((meta, _)) = url.split_once(',') {
        if url.starts_with("data:") {
            let size = estimate_data_uri_bytes(url)
                .map(|bytes| format!("{:.1} KB", bytes as f64 / 1024.0))
                .unwrap_or_else(|| "? KB".to_string());

            return format!("{meta},<image: {size}>");
        }
    }

    url.to_string()
}

fn decode_image_bytes(url: &str, raw_bytes: &[u8]) -> Option<Vec<u8>> {
    if !raw_bytes.is_empty() {
        return Some(raw_bytes.to_vec());
    }

    if url.starts_with("data:") {
        let (meta, payload) = url.split_once(',')?;

        if !meta.contains(";base64") {
            return None;
        }

        return STANDARD.decode(payload).ok();
    }

    // Zwykły URL nie zawiera bajtów obrazu.
    None
}

/// Tworzy tekstową, monochromatyczną wersję obrazu.
///
/// `raw_bytes` powinny zawierać bajty pliku PNG, JPEG lub innego
/// formatu obsługiwanego przez bibliotekę `image`.
///
/// Jeśli `raw_bytes` są puste, funkcja próbuje odczytać obraz
/// z data URI zakodowanego w base64.
///
/// Zwraca opis i wiersze znaków przedstawiające obraz.
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

    let w = width.clamp(16, 160) as u32;
    let max_h = height.clamp(6, 64) as u32;

    // Zachowaj proporcje zdjęcia. Znak w terminalu jest zwykle
    // około dwa razy wyższy niż szerszy, stąd mnożnik 0.5.
    let src_w = image.width();
    let src_h = image.height();

    if src_w == 0 || src_h == 0 {
        return None;
    }

    let natural_h =
        (w as f32 * src_h as f32 / src_w as f32 * 0.5).round() as u32;
    let h = natural_h.clamp(6, max_h);

    let gray: GrayImage = image
        .resize_exact(w, h, FilterType::Lanczos3)
        .to_luma8();

    let mut lines = Vec::with_capacity(h as usize);

    for y in 0..h {
        let mut row = String::with_capacity(w as usize);

        for x in 0..w {
            let luminance = gray.get_pixel(x, y)[0] as usize;
            let shade_index = luminance * (BW_PIXELS.len() - 1) / 255;
            row.push(BW_PIXELS[shade_index]);
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
        "1980 B&W - ",
        safe_label.as_str(),
        " - ",
        size_text.as_str(),
    ]
    .concat();

    Some((description, lines))
}
