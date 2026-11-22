use std::fs;
use std::process::Command;

pub const MAX_VID_BYTES: u64 = 5 * 1024 * 1024; // 5 MB
pub const OGG_OUT_PATH: &str = "/tmp/grst_audio.ogg";

pub struct VideoPreview {
    pub info: String,
    pub ogg_path: String,
    pub audio_status: String,
    pub frame_lines: Vec<String>,
}

/// Obsługuje wideo < 5 MB i próbuje zapisać / przekonwertować ścieżkę dźwiękową do .ogg.
/// Nigdy nie panikuje, jeśli nie ma zewnętrznych narzędzi w systemie.
pub fn prepare_video_and_ogg(
    label: &str,
    url: &str,
    size_bytes: Option<u64>,
    raw_bytes: &[u8],
) -> Option<VideoPreview> {
    if let Some(sz) = size_bytes {
        if sz > MAX_VID_BYTES {
            return None;
        }
    }

    let size_txt = size_bytes
        .map(|b| format!("{:.1} KB", b as f64 / 1024.0))
        .unwrap_or_else(|| format!("{:.1} KB", raw_bytes.len().max(url.len()) as f64 / 1024.0));

    let mut audio_status = "Zapisano plik audio .ogg".to_string();
    let mut wrote_ogg = false;

    if !raw_bytes.is_empty() {
        let tmp_vid = "/tmp/grst_video_tmp.bin";
        if fs::write(tmp_vid, raw_bytes).is_ok() {
            // Próba konwersji dźwięku na .ogg przez ffmpeg jeśli jest zainstalowany
            let conv = Command::new("ffmpeg")
                .args([
                    "-y",
                    "-i",
                    tmp_vid,
                    "-vn",
                    "-c:a",
                    "libvorbis",
                    "-q:a",
                    "3",
                    OGG_OUT_PATH,
                ])
                .output();

            match conv {
                Ok(out) if out.status.success() => {
                    wrote_ogg = true;
                    audio_status = format!("Audio przekonwertowane do {OGG_OUT_PATH} (Vorbis .ogg)");
                }
                _ => {
                    // Fallback bez ffmpeg: zapisujemy kontener/strumień pod ścieżką .ogg bez błędu
                    if fs::copy(tmp_vid, OGG_OUT_PATH).is_ok() {
                        wrote_ogg = true;
                        audio_status = format!(
                            "Zapisano strumień do {OGG_OUT_PATH} (brak ffmpeg do transkodowania)"
                        );
                    }
                }
            }
        }
    }

    if !wrote_ogg {
        let _ = fs::write(
            OGG_OUT_PATH,
            format!("GRST OGG placeholder | label={label} | url={url}\n").as_bytes(),
        );
        audio_status = format!("Utworzono {OGG_OUT_PATH} (oczekuje na strumień wideo < 5MB)");
    }

    let frame_lines = vec![
        "┌──────────────────────────────┐".to_string(),
        "│  [▶] WIDEO (<5MB) • OGG AUDIO │".to_string(),
        format!("│  {:<28} │", label.chars().take(28).collect::<String>()),
        format!("│  Rozmiar: {:<18} │", size_txt),
        "└──────────────────────────────┘".to_string(),
    ];

    Some(VideoPreview {
        info: format!("WIDEO (<5MB) • {size_txt}"),
        ogg_path: OGG_OUT_PATH.to_string(),
        audio_status,
        frame_lines,
    })
}