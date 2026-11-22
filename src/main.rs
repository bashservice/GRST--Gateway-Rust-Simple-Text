mod forms;
mod img_retro;
mod video_ogg;

use forms::FormState;
use img_retro::{format_display_url, render_retro_bw_image, MAX_IMG_BYTES};
use std::collections::HashMap;
use std::env;
use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::net::TcpStream;
use std::os::unix::fs::OpenOptionsExt;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use video_ogg::{prepare_video_and_ogg, MAX_VID_BYTES};

const PROXY_ADDR: &str = "127.0.0.1:8080";
const PROXY_WORK_DIR: &str = "/media/share/GRST/grst";
const PROXY_TMP_BIN: &str = "/tmp/grst-proxy";
static PROXY_BIN_BYTES: &[u8] = include_bytes!("../../grst-proxy");


// Kolory ANSI w stylu Midnight Commander
const MC_BG: &str = "\x1b[44m";
const MC_FG: &str = "\x1b[97m";
const MC_BORDER: &str = "\x1b[96m";
const MC_TITLE: &str = "\x1b[93;1m";
const MC_HEADER_BAR: &str = "\x1b[46;30m";
const MC_CURSOR: &str = "\x1b[7m";
const MC_LINK: &str = "\x1b[4;97m";
const MC_LINK_CURSOR: &str = "\x1b[4;7;1m";
const MC_MEDIA_BOX: &str = "\x1b[92m";
const MC_KEY_NUM: &str = "\x1b[40;97m";
const MC_KEY_LBL: &str = "\x1b[46;30m";
const ANSI_RESET: &str = "\x1b[0m";

struct ProxyGuard {
    child: Option<Child>,
}

impl ProxyGuard {
    fn start() -> Self {
        if PROXY_BIN_BYTES.is_empty() {
            return Self { child: None };
        }

        let write_res = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .mode(0o755)
            .open(PROXY_TMP_BIN)
            .and_then(|mut f| f.write_all(PROXY_BIN_BYTES));

        if write_res.is_err() {
            return Self { child: None };
        }

        // Upewniamy się, że bit wykonywalny jest ustawiony nawet przy nadpisaniu pliku
        let _ = Command::new("chmod").args(["0755", PROXY_TMP_BIN]).status();

        let mut cmd = Command::new(PROXY_TMP_BIN);
        if fs::metadata(PROXY_WORK_DIR).is_ok() {
            cmd.current_dir(PROXY_WORK_DIR);
        }
        cmd.stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());

        let child = cmd.spawn().ok();
        if child.is_some() {
            thread::sleep(Duration::from_millis(150));
        }

        Self { child }
    }
}

impl Drop for ProxyGuard {
    fn drop(&mut self) {
        if let Some(mut ch) = self.child.take() {
            let _ = ch.kill();
            let _ = ch.wait();
        }
    }
}

struct TerminalGuard {
    orig_stty: Option<String>,
}

impl TerminalGuard {
    fn enter() -> Self {
        let orig_stty = Command::new("stty")
            .arg("-g")
            .stdin(Stdio::inherit())
            .output()
            .ok()
            .and_then(|o| {
                if o.status.success() {
                    Some(String::from_utf8_lossy(&o.stdout).trim().to_string())
                } else {
                    None
                }
            });

        let _ = Command::new("stty")
            .args(["raw", "-echo"])
            .stdin(Stdio::inherit())
            .status();

        print!("\x1b[?1049h\x1b[?25l");
        let _ = io::stdout().flush();

        Self { orig_stty }
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        print!("{ANSI_RESET}\x1b[?25h\x1b[?1049l");
        let _ = io::stdout().flush();
        if let Some(ref state) = self.orig_stty {
            let _ = Command::new("stty")
                .arg(state)
                .stdin(Stdio::inherit())
                .status();
        } else {
            let _ = Command::new("stty")
                .arg("sane")
                .stdin(Stdio::inherit())
                .status();
        }
    }
}

fn get_terminal_size() -> (usize, usize) {
    if let Ok(out) = Command::new("stty")
        .arg("size")
        .stdin(Stdio::inherit())
        .output()
    {
        if out.status.success() {
            let s = String::from_utf8_lossy(&out.stdout);
            let mut parts = s.split_whitespace();
            if let (Some(r), Some(c)) = (parts.next(), parts.next()) {
                if let (Ok(rows), Ok(cols)) = (r.parse::<usize>(), c.parse::<usize>()) {
                    return (cols.max(44), rows.max(14));
                }
            }
        }
    }
    (80, 24)
}

fn url_encode(input: &str) -> String {
    let mut out = String::with_capacity(input.len() * 2);
    for b in input.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            _ => out.push_str(&format!("%{:02X}", b)),
        }
    }
    out
}

fn decode_chunked(raw: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut pos = 0;

    while pos < raw.len() {
        let Some(rel_end) = raw[pos..].windows(2).position(|w| w == b"\r\n") else {
            break;
        };
        let size_line = String::from_utf8_lossy(&raw[pos..pos + rel_end]);
        let hex_str = size_line.split(';').next().unwrap_or("").trim();
        let Ok(chunk_size) = usize::from_str_radix(hex_str, 16) else {
            break;
        };
        pos += rel_end + 2;
        if chunk_size == 0 || pos + chunk_size > raw.len() {
            break;
        }
        out.extend_from_slice(&raw[pos..pos + chunk_size]);
        pos += chunk_size;
        if raw.get(pos..pos + 2) == Some(b"\r\n") {
            pos += 2;
        }
    }
    out
}

fn http_request_proxy(
    method: &str,
    path_and_query: &str,
) -> Result<(u16, HashMap<String, String>, Vec<u8>), String> {
    let mut stream = TcpStream::connect(PROXY_ADDR)
        .map_err(|e| format!("Brak połączenia z proxy ({PROXY_ADDR}): {e}"))?;
    let _ = stream.set_read_timeout(Some(Duration::from_secs(15)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(5)));

    let request = format!(
        "{method} {path_and_query} HTTP/1.1\r\nHost: localhost:8080\r\nConnection: close\r\nUser-Agent: GRST-MC/0.9\r\n\r\n"
    );
    stream
        .write_all(request.as_bytes())
        .map_err(|e| format!("Błąd wysyłania żądania: {e}"))?;

    let mut raw_response = Vec::new();
    stream
        .read_to_end(&mut raw_response)
        .map_err(|e| format!("Błąd odczytu odpowiedzi: {e}"))?;

    let header_end = raw_response
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or_else(|| "Nieprawidłowa odpowiedź HTTP z proxy".to_string())?;

    let headers_str = String::from_utf8_lossy(&raw_response[..header_end]);
    let body_bytes = &raw_response[header_end + 4..];

    let mut lines = headers_str.lines();
    let status_line = lines.next().unwrap_or("");
    let status_code = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse::<u16>().ok())
        .unwrap_or(0);

    let mut headers = HashMap::new();
    for line in lines {
        if let Some((k, v)) = line.split_once(':') {
            headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
        }
    }

    let is_chunked = headers
        .get("transfer-encoding")
        .map(|v| v.to_ascii_lowercase().contains("chunked"))
        .unwrap_or(false);

    let decoded = if is_chunked && method != "HEAD" {
        decode_chunked(body_bytes)
    } else {
        body_bytes.to_vec()
    };

    Ok((status_code, headers, decoded))
}

fn http_get_proxy(path_and_query: &str) -> Result<(u16, String), String> {
    let (code, _, body) = http_request_proxy("GET", path_and_query)?;
    Ok((code, String::from_utf8_lossy(&body).into_owned()))
}

struct LoadingAnim {
    style_id: usize,
    tick: usize,
}

impl LoadingAnim {
    fn random_new() -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.subsec_nanos() as usize)
            .unwrap_or(0);
        Self {
            style_id: nanos % 3,
            tick: 0,
        }
    }

    fn step(&mut self) {
        self.tick = self.tick.wrapping_add(1);
    }

    fn spinner_char(&self) -> String {
        match self.style_id {
            0 => {
                const FRAMES: [char; 4] = ['|', '/', '-', '\\'];
                FRAMES[self.tick % FRAMES.len()].to_string()
            }
            1 => {
                const FRAMES: [&str; 4] = ["[=   ]", "[ ==  ]", "[  == ]", "[   ==]"];
                FRAMES[self.tick % FRAMES.len()].to_string()
            }
            _ => {
                const FRAMES: [&str; 4] = [".  ", ".. ", "...", " .."];
                FRAMES[self.tick % FRAMES.len()].to_string()
            }
        }
    }

    fn percent(&self) -> u8 {
        ((self.tick * 4).min(95)) as u8
    }
}

#[derive(Clone, Copy, PartialEq)]
enum MediaKind {
    None,
    Image,
    Video,
}

impl MediaKind {
    fn inline_icon(self) -> &'static str {
        match self {
            MediaKind::Image => "[IMG] ",
            MediaKind::Video => "[VID] ",
            MediaKind::None => "",
        }
    }
}

#[derive(Clone)]
struct LinkItem {
    num: usize,
    label: String,
    url: String,
    media_kind: MediaKind,
    size_bytes: Option<u64>,
}

fn detect_media_kind_by_url(url: &str) -> MediaKind {
    let low = url.to_ascii_lowercase();
    if low.starts_with("data:image/") {
        return MediaKind::Image;
    }
    if low.starts_with("data:video/") {
        return MediaKind::Video;
    }

    let base = low.split('?').next().unwrap_or(&low);
    if base.ends_with(".png")
        || base.ends_with(".jpg")
        || base.ends_with(".jpeg")
        || base.ends_with(".gif")
        || base.ends_with(".webp")
        || base.ends_with(".svg")
        || base.ends_with(".bmp")
        || base.contains("ptv-imported-images")
        || base.contains("/images/")
    {
        MediaKind::Image
    } else if base.ends_with(".mp4")
        || base.ends_with(".webm")
        || base.ends_with(".mkv")
        || base.ends_with(".mov")
        || base.ends_with(".avi")
        || base.ends_with(".m3u8")
    {
        MediaKind::Video
    } else {
        MediaKind::None
    }
}

#[derive(Clone)]
enum TextSpan {
    Plain(String),
    Link { link_idx: usize, label: String },
}

fn is_supported_target(url: &str) -> bool {
    let low = url.to_ascii_lowercase();
    low.starts_with("http://")
        || low.starts_with("https://")
        || low.starts_with("data:image/")
        || low.starts_with("data:video/")
}

fn parse_document(text: &str) -> (Vec<TextSpan>, Vec<LinkItem>) {
    let mut spans = Vec::new();
    let mut links = Vec::new();
    let mut next_auto_num = 1usize;
    let mut rest = text;

    while let Some(open_idx) = rest.find('[') {
        let before = &rest[..open_idx];
        let after_open = &rest[open_idx + 1..];

        let Some(close_idx) = after_open.find(']') else {
            spans.push(TextSpan::Plain(rest.to_string()));
            return (spans, links);
        };

        let token = &after_open[..close_idx];
        let mut parsed: Option<(usize, String, String, MediaKind)> = None;

        if let Some((prefix, label_and_url)) = token.split_once(':') {
            if let Some((label, url)) = label_and_url.rsplit_once("->") {
                let clean_url: String = if url.trim_start().to_ascii_lowercase().starts_with("data:") {
                    url.chars().filter(|c| !c.is_whitespace()).collect()
                } else {
                    url.trim().to_string()
                };

                if is_supported_target(&clean_url) {
                    let p_up = prefix.trim().to_ascii_uppercase();
                    let (num, forced_kind) = if let Ok(n) = prefix.trim().parse::<usize>() {
                        (n, MediaKind::None)
                    } else if p_up == "OBRAZ" || p_up == "IMG" || p_up == "IMAGE" {
                        let n = next_auto_num;
                        next_auto_num += 1;
                        (n, MediaKind::Image)
                    } else if p_up == "VIDEO" || p_up == "WIDEO" {
                        let n = next_auto_num;
                        next_auto_num += 1;
                        (n, MediaKind::Video)
                    } else if p_up == "LINK" || p_up == "URL" {
                        let n = next_auto_num;
                        next_auto_num += 1;
                        (n, MediaKind::None)
                    } else {
                        (0, MediaKind::None)
                    };

                    if num > 0 {
                        let kind = if forced_kind != MediaKind::None {
                            forced_kind
                        } else {
                            detect_media_kind_by_url(&clean_url)
                        };
                        let raw_lbl = label.trim();
                        let base_lbl = if raw_lbl.is_empty() {
                            if clean_url.starts_with("data:") {
                                "osadzony plik".to_string()
                            } else {
                                clean_url.clone()
                            }
                        } else {
                            raw_lbl.to_string()
                        };
                        let display_lbl = format!("{}{}", kind.inline_icon(), base_lbl);
                        parsed = Some((num, display_lbl, clean_url, kind));
                    }
                }
            }
        }

        if let Some((num, display_lbl, clean_url, kind)) = parsed {
            if !before.is_empty() {
                spans.push(TextSpan::Plain(before.to_string()));
            }
            let link_idx = links.len();
            spans.push(TextSpan::Link {
                link_idx,
                label: display_lbl.clone(),
            });
            links.push(LinkItem {
                num,
                label: display_lbl,
                url: clean_url,
                media_kind: kind,
                size_bytes: None,
            });
        } else {
            let raw_chunk = &rest[..open_idx + 1 + close_idx + 1];
            spans.push(TextSpan::Plain(raw_chunk.to_string()));
        }

        rest = &after_open[close_idx + 1..];
    }

    if !rest.is_empty() {
        spans.push(TextSpan::Plain(rest.to_string()));
    }

    (spans, links)
}

#[derive(Clone)]
struct StyledRun {
    text: String,
    link_idx: Option<usize>,
    is_first_word_of_link: bool,
}

fn flatten_vertical_menus(spans: &[TextSpan]) -> Vec<TextSpan> {
    struct Entry {
        text: String,
        link_idx: Option<usize>,
        is_menu_candidate: bool,
    }

    let mut entries: Vec<Entry> = Vec::new();
    for span in spans {
        let (raw, l_idx) = match span {
            TextSpan::Plain(t) => (t.as_str(), None),
            TextSpan::Link { link_idx, label } => (label.as_str(), Some(*link_idx)),
        };
        let parts: Vec<&str> = raw.split('\n').collect();
        for (i, part) in parts.iter().enumerate() {
            let trimmed = part.trim();
            if !trimmed.is_empty() {
                let word_count = trimmed.split_whitespace().count();
                let char_count = trimmed.chars().count();
                let is_cand = word_count <= 3 && char_count <= 26;
                entries.push(Entry {
                    text: trimmed.to_string(),
                    link_idx: l_idx,
                    is_menu_candidate: is_cand,
                });
            }
            if i + 1 < parts.len() && trimmed.is_empty() {
                entries.push(Entry {
                    text: "\n".to_string(),
                    link_idx: None,
                    is_menu_candidate: false,
                });
            }
        }
    }

    let mut out: Vec<TextSpan> = Vec::new();
    let mut i = 0usize;
    while i < entries.len() {
        if entries[i].text == "\n" {
            out.push(TextSpan::Plain("\n".to_string()));
            i += 1;
            continue;
        }

        let mut j = i;
        while j < entries.len() && entries[j].is_menu_candidate {
            j += 1;
        }

        let run_len = j - i;
        if run_len >= 4 {
            for k in i..j {
                if k > i {
                    out.push(TextSpan::Plain(" │ ".to_string()));
                }
                match entries[k].link_idx {
                    Some(link_idx) => out.push(TextSpan::Link {
                        link_idx,
                        label: entries[k].text.clone(),
                    }),
                    None => out.push(TextSpan::Plain(entries[k].text.clone())),
                }
            }
            out.push(TextSpan::Plain("\n".to_string()));
            i = j;
        } else {
            match entries[i].link_idx {
                Some(link_idx) => out.push(TextSpan::Link {
                    link_idx,
                    label: entries[i].text.clone(),
                }),
                None => out.push(TextSpan::Plain(entries[i].text.clone())),
            }
            out.push(TextSpan::Plain("\n".to_string()));
            i += 1;
        }
    }

    out
}

fn wrap_spans(spans: &[TextSpan], max_width: usize) -> Vec<Vec<StyledRun>> {
    let normalized = flatten_vertical_menus(spans);
    let w = max_width.max(12);
    let mut lines: Vec<Vec<StyledRun>> = vec![Vec::new()];
    let mut cur_w = 0usize;
    let mut seen_link_first_word: Vec<bool> = vec![false; 2048];

    let push_token = |lines: &mut Vec<Vec<StyledRun>>,
                      cur_w: &mut usize,
                      token: &str,
                      link_idx: Option<usize>,
                      is_first_word: bool| {
        let t_len = token.chars().count();
        if t_len == 0 {
            return;
        }
        if *cur_w > 0 && *cur_w + t_len > w {
            lines.push(Vec::new());
            *cur_w = 0;
        }
        if t_len > w {
            for (idx_c, ch) in token.chars().enumerate() {
                if *cur_w >= w {
                    lines.push(Vec::new());
                    *cur_w = 0;
                }
                let line = lines.last_mut().unwrap();
                let fw = is_first_word && idx_c == 0;
                if let Some(last) = line.last_mut() {
                    if last.link_idx == link_idx && last.is_first_word_of_link == fw {
                        last.text.push(ch);
                    } else {
                        line.push(StyledRun {
                            text: ch.to_string(),
                            link_idx,
                            is_first_word_of_link: fw,
                        });
                    }
                } else {
                    line.push(StyledRun {
                        text: ch.to_string(),
                        link_idx,
                        is_first_word_of_link: fw,
                    });
                }
                *cur_w += 1;
            }
            return;
        }

        let line = lines.last_mut().unwrap();
        if let Some(last) = line.last_mut() {
            if last.link_idx == link_idx && last.is_first_word_of_link == is_first_word {
                last.text.push_str(token);
            } else {
                line.push(StyledRun {
                    text: token.to_string(),
                    link_idx,
                    is_first_word_of_link: is_first_word,
                });
            }
        } else {
            line.push(StyledRun {
                text: token.to_string(),
                link_idx,
                is_first_word_of_link: is_first_word,
            });
        }
        *cur_w += t_len;
    };

    for span in &normalized {
        let (raw_txt, link_idx) = match span {
            TextSpan::Plain(t) => (t.as_str(), None),
            TextSpan::Link { link_idx, label } => (label.as_str(), Some(*link_idx)),
        };

        let mut is_first_word_in_span = true;
        let mut parts = raw_txt.split('\n').peekable();
        while let Some(part) = parts.next() {
            for word in part.split_whitespace() {
                if cur_w > 0 && cur_w + 1 + word.chars().count() <= w {
                    push_token(&mut lines, &mut cur_w, " ", link_idx, false);
                } else if cur_w > 0 {
                    lines.push(Vec::new());
                    cur_w = 0;
                }

                let mut mark_first = false;
                if let Some(l_i) = link_idx {
                    if is_first_word_in_span {
                        if l_i >= seen_link_first_word.len() {
                            seen_link_first_word.resize(l_i + 64, false);
                        }
                        if !seen_link_first_word[l_i] {
                            seen_link_first_word[l_i] = true;
                            mark_first = true;
                        }
                        is_first_word_in_span = false;
                    }
                }

                push_token(&mut lines, &mut cur_w, word, link_idx, mark_first);
            }
            if parts.peek().is_some() {
                lines.push(Vec::new());
                cur_w = 0;
            }
        }
    }

    if lines.is_empty() {
        lines.push(Vec::new());
    }
    lines
}

fn normalize_url(input: &str) -> String {
    let trimmed = input.trim();
    if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
        trimmed.to_string()
    } else {
        format!("https://{trimmed}")
    }
}

fn fit_str(s: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    let mut out = String::new();
    let mut count = 0;
    for ch in s.chars() {
        if ch == '\t' {
            let spaces = 4.min(width.saturating_sub(count));
            out.push_str(&" ".repeat(spaces));
            count += spaces;
        } else if !ch.is_control() {
            if count + 1 > width {
                break;
            }
            out.push(ch);
            count += 1;
        }
        if count >= width {
            break;
        }
    }
    if count < width {
        out.push_str(&" ".repeat(width - count));
    }
    out
}

#[derive(PartialEq, Clone, Copy)]
enum FocusPane {
    Content,
    Links,
}

#[derive(PartialEq, Clone, Copy)]
enum InputMode {
    Normal,
    GotoUrl,
    Search,
    FieldText,
}

struct MediaModal {
    title: String,
    subtitle: String,
    lines: Vec<String>,
    url: String,
    raw_bytes: Vec<u8>,
    size_bytes: Option<u64>,
    scale_level: i8,
}

struct App {
    current_url: String,
    history: Vec<String>,
    spans: Vec<TextSpan>,
    links: Vec<LinkItem>,
    link_map: HashMap<usize, String>,
    scroll_y: usize,
    selected_link: usize,
    focus: FocusPane,
    input_mode: InputMode,
    input_buf: String,
    num_buf: String,
    forms: FormState,
    status_msg: String,
    media_modal: Option<MediaModal>,
    loading: Option<LoadingAnim>,
}

impl App {
    fn new(start_url: String) -> Self {
        Self {
            current_url: start_url,
            history: Vec::new(),
            spans: Vec::new(),
            links: Vec::new(),
            link_map: HashMap::new(),
            scroll_y: 0,
            selected_link: 0,
            focus: FocusPane::Content,
            input_mode: InputMode::Normal,
            input_buf: String::new(),
            num_buf: String::new(),
            forms: FormState::new(),
            status_msg: "Gotowy".to_string(),
            media_modal: None,
            loading: None,
        }
    }

    fn fetch_media_bytes(&self, url: &str) -> (Option<u64>, Vec<u8>) {
        if url.starts_with("data:") {
            let sz = img_retro::estimate_data_uri_bytes(url);
            return (sz, Vec::new());
        }
        let query = format!("/fetch?url={}", url_encode(url));
        if let Ok((code, headers, body)) = http_request_proxy("GET", &query) {
            if (200..300).contains(&code) {
                let len = headers
                    .get("content-length")
                    .and_then(|v| v.parse::<u64>().ok())
                    .or(Some(body.len() as u64));
                return (len, body);
            }
        }
        (None, Vec::new())
    }

    fn mark_link_oversized(&mut self, num: usize, mb_txt: &str) {
        for l in &mut self.links {
            if l.num == num && !l.label.contains("[! >1MB]") {
                l.label = format!("{} [! >1MB ({mb_txt})]", l.label);
            }
        }
    }

    fn rebuild_image_modal(&mut self) {
        let Some(ref mut modal) = self.media_modal else {
            return;
        };
        if modal.raw_bytes.is_empty() && !modal.url.starts_with("data:") {
            return;
        }

        let (cols, rows) = get_terminal_size();
        let (art_w, art_h) = match modal.scale_level {
            -1 => (36usize, 10usize),
            1 => (
                cols.saturating_sub(4).min(80),
                rows.saturating_sub(10).min(24),
            ),
            _ => (
                cols.saturating_sub(8).min(64),
                rows.saturating_sub(14).min(18),
            ),
        };

        if let Some((info, bw_lines)) = render_retro_bw_image(
            &modal.subtitle,
            &modal.url,
            modal.size_bytes,
            &modal.raw_bytes,
            art_w,
            art_h,
        ) {
            modal.title = format!("1980 B&W PIXEL: {info}");
            modal.lines = bw_lines;
        }
    }

    fn open_media_or_link(&mut self, mut item: LinkItem) {
        let (cols, rows) = get_terminal_size();
        let art_w = cols.saturating_sub(8).min(64);
        let art_h = rows.saturating_sub(14).min(18);

        match item.media_kind {
            MediaKind::Image => {
                let (sz, raw) = self.fetch_media_bytes(&item.url);
                item.size_bytes = sz;
                if let Some(b) = sz {
                    if b > MAX_IMG_BYTES {
                        let mb_txt = format!("{:.2} MB", b as f64 / (1024.0 * 1024.0));
                        self.mark_link_oversized(item.num, &mb_txt);
                        self.status_msg = format!("! Obraz przekracza 1 MB ({mb_txt})");
                        return;
                    }
                }
                if raw.is_empty() && !item.url.starts_with("data:") {
                    self.status_msg = "! Brak bajtów obrazu z proxy (sprawdź /fetch)".to_string();
                    return;
                }
                if let Some((info, bw_lines)) =
                    render_retro_bw_image(&item.label, &item.url, sz, &raw, art_w, art_h)
                {
                    self.media_modal = Some(MediaModal {
                        title: format!("1980 B&W PIXEL: {info}"),
                        subtitle: item.label.clone(),
                        lines: bw_lines,
                        url: format_display_url(&item.url),
                        raw_bytes: raw,
                        size_bytes: sz,
                        scale_level: 0,
                    });
                    self.status_msg =
                        "Skala: [-] mniejszy, [+] większy, [z] przełącz, [Esc/Enter] zamknij"
                            .to_string();
                } else {
                    let kb = sz
                        .map(|b| format!(" ({:.1} KB)", b as f64 / 1024.0))
                        .unwrap_or_default();
                    self.status_msg = format!(
                        "! Nie udało się przekonwertować obrazu{kb} (plik >1 MB lub zły format)"
                    );
                }
                return;
            }
            MediaKind::Video => {
                let (sz, raw) = self.fetch_media_bytes(&item.url);
                item.size_bytes = sz;
                if sz.map(|b| b > MAX_VID_BYTES).unwrap_or(false) {
                    self.status_msg = "Wideo przekracza 5 MB".to_string();
                    return;
                }
                if let Some(vp) = prepare_video_and_ogg(&item.label, &item.url, sz, &raw) {
                    let mut combined = vp.frame_lines;
                    combined.push(vp.audio_status);
                    self.media_modal = Some(MediaModal {
                        title: format!("WIDEO + OGG: {}", vp.info),
                        subtitle: format!("Ścieżka audio: {}", vp.ogg_path),
                        lines: combined,
                        url: format_display_url(&item.url),
                        raw_bytes: Vec::new(),
                        size_bytes: None,
                        scale_level: 0,
                    });
                    self.status_msg = format!("Audio zapisane w {}", vp.ogg_path);
                } else {
                    self.status_msg = "Nie udało się przygotować wideo / .ogg".to_string();
                }
                return;
            }
            MediaKind::None => {}
        }

        if item.url.starts_with("data:") {
            self.status_msg = "Osadzone dane data: nie są adresem strony".to_string();
            return;
        }
        self.load_page_animated(&item.url, true);
    }

    fn load_page_animated(&mut self, target: &str, push_history: bool) {
        self.media_modal = None;
        let mut anim = LoadingAnim::random_new();
        let target_owned = target.to_string();
        let query = format!("/fetch?url={}&raw=1", url_encode(&target_owned));

        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let res = http_get_proxy(&query);
            let _ = tx.send(res);
        });

        loop {
            anim.step();
            let pct = anim.percent();
            let sp = anim.spinner_char();
            self.status_msg = format!("Ładowanie {sp} {pct}% : {target_owned}");
            self.loading = Some(LoadingAnim {
                style_id: anim.style_id,
                tick: anim.tick,
            });
            self.render();

            match rx.recv_timeout(Duration::from_millis(80)) {
                Ok(result) => {
                    self.loading = None;
                    match result {
                        Ok((200, body)) => {
                            if push_history && self.current_url != target_owned {
                                self.history.push(self.current_url.clone());
                            }
                            self.current_url = target_owned;
                            let (spans, links) = parse_document(&body);
                            self.link_map.clear();
                            for item in &links {
                                self.link_map.insert(item.num, item.url.clone());
                            }
                            self.spans = spans;
                            self.links = links;
                            self.scroll_y = 0;
                            self.selected_link = 0;
                            self.num_buf.clear();
                            self.status_msg = format!(
                                "100% Załadowano ({} ukrytych linków)",
                                self.links.len()
                            );
                        }
                        Ok((code, body)) => {
                            self.status_msg = format!(
                                "Błąd HTTP {code}: {}",
                                body.lines().next().unwrap_or("")
                            );
                        }
                        Err(err) => {
                            self.status_msg = err;
                        }
                    }
                    break;
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    self.status_msg = "Błąd wątku ładowania".to_string();
                    break;
                }
            }
        }
    }

    fn load_stats(&mut self) {
        self.media_modal = None;
        match http_get_proxy("/stats") {
            Ok((200, stats_body)) => {
                self.history.push(self.current_url.clone());
                self.current_url = "grst://stats".to_string();
                let txt = format!(
                    "=== STATYSTYKI OSZCZĘDNOŚCI GRST ===\n\n{}",
                    stats_body.trim()
                );
                self.spans = vec![TextSpan::Plain(txt)];
                self.links.clear();
                self.link_map.clear();
                self.scroll_y = 0;
                self.selected_link = 0;
                self.status_msg = "Wyświetlono /stats".to_string();
            }
            Ok((code, _)) => self.status_msg = format!("Błąd /stats HTTP {code}"),
            Err(e) => self.status_msg = e,
        }
    }

    fn render(&self) {
        let (cols, rows) = get_terminal_size();
        let mut frame = String::with_capacity(cols * rows * 3);

        frame.push_str("\x1b[H");
        frame.push_str(MC_BG);
        frame.push_str(MC_FG);

        let form_sum = self.forms.summary();
        let top_title = if let Some(ref anim) = self.loading {
            format!(
                " GRST-MC │ {} {:>3}% │ {} ",
                anim.spinner_char(),
                anim.percent(),
                self.current_url
            )
        } else if !form_sum.is_empty() {
            format!(" GRST-MC │ {} │ Pola: {} ", self.current_url, form_sum)
        } else {
            format!(
                " GRST-MC │ {} │ Linki: {} │ Historia: {}",
                self.current_url,
                self.links.len(),
                self.history.len()
            )
        };
        frame.push_str(MC_HEADER_BAR);
        frame.push_str(&fit_str(&top_title, cols));
        frame.push_str(MC_BG);
        frame.push_str("\r\n");

        let link_panel_rows = 5usize;
        let main_panel_rows = rows.saturating_sub(1 + link_panel_rows + 2).max(4);
        let inner_w = cols.saturating_sub(2);

        let wrapped = wrap_spans(&self.spans, inner_w);
        let max_scroll = wrapped.len().saturating_sub(main_panel_rows.saturating_sub(2));
        let scroll = self.scroll_y.min(max_scroll);

        let cursor_link_idx = if self.focus == FocusPane::Links {
            Some(self.selected_link)
        } else {
            None
        };

        let content_title = format!(" Treść strony [{}/{}] ", scroll + 1, wrapped.len());
        let title_col = if self.focus == FocusPane::Content {
            MC_TITLE
        } else {
            MC_FG
        };

        frame.push_str(MC_BORDER);
        frame.push('┌');
        frame.push_str(title_col);
        let t_fit = fit_str(&content_title, content_title.chars().count().min(inner_w));
        frame.push_str(&t_fit);
        frame.push_str(MC_BORDER);
        frame.push_str(&"─".repeat(inner_w.saturating_sub(t_fit.chars().count())));
        frame.push_str("┐\r\n");

        let body_visible_rows = main_panel_rows.saturating_sub(2);
        for i in 0..body_visible_rows {
            frame.push_str(MC_BORDER);
            frame.push('│');

            if let Some(ref modal) = self.media_modal {
                frame.push_str(MC_MEDIA_BOX);
                let line_txt = if i == 0 {
                    format!(" === {} ===", modal.title)
                } else if i == 1 {
                    format!(" {}", modal.subtitle)
                } else if i >= 3 && i < 3 + modal.lines.len() {
                    format!("   {}", modal.lines[i - 3])
                } else if i + 2 == body_visible_rows {
                    format!(" URL: {}", modal.url)
                } else if i + 1 == body_visible_rows {
                    " [- / z] mniejszy, [+] większy, [Esc/Enter] zamknij.".to_string()
                } else {
                    String::new()
                };
                frame.push_str(&fit_str(&line_txt, inner_w));
            } else {
                frame.push_str(MC_FG);
                let mut used_chars = 0usize;
                if let Some(runs) = wrapped.get(scroll + i) {
                    for run in runs {
                        let rem = inner_w.saturating_sub(used_chars);
                        if rem == 0 {
                            break;
                        }
                        let visible: String = run.text.chars().take(rem).collect();
                        let v_len = visible.chars().count();
                        match run.link_idx {
                            Some(idx) if Some(idx) == cursor_link_idx && run.is_first_word_of_link => {
                                frame.push_str(MC_LINK_CURSOR);
                                frame.push_str(&visible);
                                frame.push_str("\x1b[27;24;22m");
                                frame.push_str(MC_BG);
                                frame.push_str(MC_FG);
                            }
                            Some(_) => {
                                frame.push_str(MC_LINK);
                                frame.push_str(&visible);
                                frame.push_str("\x1b[24m");
                                frame.push_str(MC_FG);
                            }
                            None => {
                                frame.push_str(&visible);
                            }
                        }
                        used_chars += v_len;
                    }
                }
                if used_chars < inner_w {
                    frame.push_str(&" ".repeat(inner_w - used_chars));
                }
            }

            frame.push_str(MC_BORDER);
            frame.push_str("│\r\n");
        }

        frame.push_str(MC_BORDER);
        frame.push('└');
        frame.push_str(&"─".repeat(inner_w));
        frame.push_str("┘\r\n");

        let links_title = if let Some(sel) = self.links.get(self.selected_link) {
            format!(
                " Link [{}/{}]: {} ",
                self.selected_link + 1,
                self.links.len(),
                format_display_url(&sel.url)
            )
        } else {
            " Linki (brak) ".to_string()
        };
        let links_title_col = if self.focus == FocusPane::Links {
            MC_TITLE
        } else {
            MC_FG
        };

        frame.push_str(MC_BORDER);
        frame.push('┌');
        frame.push_str(links_title_col);
        let lt_fit = fit_str(&links_title, links_title.chars().count().min(inner_w));
        frame.push_str(&lt_fit);
        frame.push_str(MC_BORDER);
        frame.push_str(&"─".repeat(inner_w.saturating_sub(lt_fit.chars().count())));
        frame.push_str("┐\r\n");

        let list_rows = link_panel_rows.saturating_sub(2);
        let link_offset = self
            .selected_link
            .saturating_sub(list_rows.saturating_sub(1));

        for i in 0..list_rows {
            let idx = link_offset + i;
            frame.push_str(MC_BORDER);
            frame.push('│');
            if let Some(item) = self.links.get(idx) {
                let row_str = format!(
                    " [{}] {} -> {}",
                    item.num,
                    item.label,
                    format_display_url(&item.url)
                );
                if self.focus == FocusPane::Links && idx == self.selected_link {
                    frame.push_str(MC_CURSOR);
                    frame.push_str(&fit_str(&row_str, inner_w));
                    frame.push_str("\x1b[27m");
                    frame.push_str(MC_BG);
                    frame.push_str(MC_FG);
                } else {
                    frame.push_str(MC_FG);
                    frame.push_str(&fit_str(&row_str, inner_w));
                }
            } else {
                frame.push_str(MC_FG);
                frame.push_str(&" ".repeat(inner_w));
            }
            frame.push_str(MC_BORDER);
            frame.push_str("│\r\n");
        }

        frame.push_str(MC_BORDER);
        frame.push('└');
        frame.push_str(&"─".repeat(inner_w));
        frame.push_str("┘\r\n");

        let status_line = match self.input_mode {
            InputMode::GotoUrl => format!(" Otwórz URL: {}█", self.input_buf),
            InputMode::Search => format!(" Szukaj na stronie: {}█", self.input_buf),
            InputMode::FieldText => {
                let fname = self
                    .forms
                    .active_field()
                    .map(|f| f.name.as_str())
                    .unwrap_or("pole");
                format!(" Wpisz [{fname}]: {}█", self.input_buf)
            }
            InputMode::Normal => {
                if !self.num_buf.is_empty() {
                    format!(" Numer linku: [{}] (Enter = otwórz, Esc = anuluj)", self.num_buf)
                } else {
                    format!(" {}", self.status_msg)
                }
            }
        };
        frame.push_str(MC_BG);
        frame.push_str(MC_TITLE);
        frame.push_str(&fit_str(&status_line, cols));
        frame.push_str("\r\n");

        let keys = [
            ("Tab", "Panel "),
            ("]/h", "+/-10Link "),
            ("Enter", "Otwórz "),
            ("g", "URL "),
            ("b", "Wstecz "),
            ("/", "Szukaj "),
            ("s", "Stats "),
            ("q", "Wyjdź "),
        ];
        let mut used = 0;
        for (k, lbl) in keys {
            if used + k.len() + lbl.len() > cols {
                break;
            }
            frame.push_str(MC_KEY_NUM);
            frame.push_str(k);
            frame.push_str(MC_KEY_LBL);
            frame.push_str(lbl);
            used += k.len() + lbl.len();
        }
        if used < cols {
            frame.push_str(MC_KEY_LBL);
            frame.push_str(&" ".repeat(cols - used));
        }

        let mut out = io::stdout().lock();
        let _ = out.write_all(frame.as_bytes());
        let _ = out.flush();
    }

    fn search_next(&mut self, query: &str) {
        if query.is_empty() {
            return;
        }
        let (cols, _) = get_terminal_size();
        let inner_w = cols.saturating_sub(2);
        let wrapped = wrap_spans(&self.spans, inner_w);
        let q_low = query.to_lowercase();

        for offset in 1..=wrapped.len() {
            let idx = (self.scroll_y + offset) % wrapped.len();
            let line_plain: String = wrapped[idx].iter().map(|r| r.text.as_str()).collect();
            if line_plain.to_lowercase().contains(&q_low) {
                self.scroll_y = idx;
                self.focus = FocusPane::Content;
                self.status_msg = format!("Znaleziono '{query}' w wierszu {}", idx + 1);
                return;
            }
        }
        self.status_msg = format!("Nie znaleziono: '{query}'");
    }
}

enum Key {
    Up,
    Down,
    Home,
    End,
    PageUp,
    PageDown,
    Tab,
    Enter,
    Backspace,
    Esc,
    Char(char),
}

fn read_key(stdin: &mut io::StdinLock) -> Option<Key> {
    let mut buf = [0u8; 1];
    if stdin.read(&mut buf).ok()? == 0 {
        return None;
    }
    match buf[0] {
        b'\r' | b'\n' => Some(Key::Enter),
        b'\t' => Some(Key::Tab),
        127 | 8 => Some(Key::Backspace),
        27 => {
            let mut seq = [0u8; 2];
            if stdin.read(&mut seq[0..1]).unwrap_or(0) == 0 {
                return Some(Key::Esc);
            }
            if seq[0] == b'[' || seq[0] == b'O' {
                if stdin.read(&mut seq[1..2]).unwrap_or(0) == 0 {
                    return Some(Key::Esc);
                }
                match seq[1] {
                    b'A' => Some(Key::Up),
                    b'B' => Some(Key::Down),
                    b'H' => Some(Key::Home),
                    b'F' => Some(Key::End),
                    b'1' | b'4' | b'5' | b'6' => {
                        let mut tilde = [0u8; 1];
                        let _ = stdin.read(&mut tilde);
                        match seq[1] {
                            b'1' => Some(Key::Home),
                            b'4' => Some(Key::End),
                            b'5' => Some(Key::PageUp),
                            b'6' => Some(Key::PageDown),
                            _ => Some(Key::Esc),
                        }
                    }
                    _ => Some(Key::Esc),
                }
            } else {
                Some(Key::Esc)
            }
        }
        b if b >= 32 => {
            let len = if b & 0b1000_0000 == 0 {
                1
            } else if b & 0b1110_0000 == 0b1100_0000 {
                2
            } else if b & 0b1111_0000 == 0b1110_0000 {
                3
            } else {
                4
            };
            let mut bytes = [0u8; 4];
            bytes[0] = b;
            for i in 1..len {
                let _ = stdin.read(&mut bytes[i..i + 1]);
            }
            std::str::from_utf8(&bytes[..len])
                .ok()
                .and_then(|s| s.chars().next())
                .map(Key::Char)
        }
        _ => None,
    }
}

fn main() {
    let start_url = env::args()
        .nth(1)
        .map(|u| normalize_url(&u))
        .unwrap_or_else(|| "https://onet.pl".to_string());

    let _proxy = ProxyGuard::start();
    let _guard = TerminalGuard::enter();
    let mut app = App::new(start_url.clone());
    app.load_page_animated(&start_url, false);

    let stdin = io::stdin();
    let mut lock = stdin.lock();

    loop {
        app.render();
        let Some(key) = read_key(&mut lock) else {
            break;
        };

        if app.input_mode != InputMode::Normal {
            match key {
                Key::Esc => {
                    app.input_mode = InputMode::Normal;
                    app.input_buf.clear();
                    app.status_msg = "Anulowano".to_string();
                }
                Key::Backspace => {
                    app.input_buf.pop();
                }
                Key::Tab if app.input_mode == InputMode::FieldText => {
                    app.forms.set_active_value(app.input_buf.trim().to_string());
                    app.forms.next_field();
                    app.input_buf = app
                        .forms
                        .active_field()
                        .map(|f| f.value.clone())
                        .unwrap_or_default();
                }
                Key::Enter => {
                    let val = app.input_buf.trim().to_string();
                    let mode = app.input_mode;
                    app.input_mode = InputMode::Normal;
                    app.input_buf.clear();
                    match mode {
                        InputMode::GotoUrl if !val.is_empty() => {
                            let target = normalize_url(&val);
                            app.load_page_animated(&target, true);
                        }
                        InputMode::Search if !val.is_empty() => app.search_next(&val),
                        InputMode::FieldText => {
                            let fname = app
                                .forms
                                .active_field()
                                .map(|f| f.name.clone())
                                .unwrap_or_else(|| "pole".to_string());
                            app.forms.set_active_value(val.clone());
                            app.status_msg = format!("Zapisano [{fname}]: '{val}'");
                        }
                        _ => {}
                    }
                }
                Key::Char(c) => app.input_buf.push(c),
                _ => {}
            }
            continue;
        }

        if app.media_modal.is_some() {
            match key {
                Key::Esc | Key::Enter | Key::Char('q') => {
                    app.media_modal = None;
                    app.status_msg = "Zamknięto podgląd mediów".to_string();
                }
                Key::Char('-') => {
                    if let Some(ref mut m) = app.media_modal {
                        m.scale_level = (m.scale_level - 1).max(-1);
                    }
                    app.rebuild_image_modal();
                }
                Key::Char('+') | Key::Char('=') => {
                    if let Some(ref mut m) = app.media_modal {
                        m.scale_level = (m.scale_level + 1).min(1);
                    }
                    app.rebuild_image_modal();
                }
                Key::Char('z') => {
                    if let Some(ref mut m) = app.media_modal {
                        m.scale_level = if m.scale_level == -1 { 0 } else { -1 };
                    }
                    app.rebuild_image_modal();
                }
                _ => {}
            }
            continue;
        }

        match key {
            Key::Char('q') => break,
            Key::Tab => {
                app.focus = match app.focus {
                    FocusPane::Content => FocusPane::Links,
                    FocusPane::Links => FocusPane::Content,
                };
            }
            Key::Char('i') => {
                app.input_mode = InputMode::FieldText;
                app.input_buf = app
                    .forms
                    .active_field()
                    .map(|f| f.value.clone())
                    .unwrap_or_default();
            }
            Key::Char('n') => {
                if app.selected_link + 1 < app.links.len() {
                    app.selected_link += 1;
                    app.focus = FocusPane::Links;
                }
            }
            Key::Char('p') => {
                if app.selected_link > 0 {
                    app.selected_link -= 1;
                    app.focus = FocusPane::Links;
                }
            }
            Key::Char(']') => {
                if !app.links.is_empty() {
                    app.selected_link = (app.selected_link + 10).min(app.links.len() - 1);
                    app.focus = FocusPane::Links;
                }
            }
            Key::Char('[') | Key::Char('h') => {
                if !app.links.is_empty() {
                    app.selected_link = app.selected_link.saturating_sub(10);
                    app.focus = FocusPane::Links;
                }
            }
            Key::Home => match app.focus {
                FocusPane::Content => app.scroll_y = 0,
                FocusPane::Links => app.selected_link = 0,
            },
            Key::End => match app.focus {
                FocusPane::Content => app.scroll_y = usize::MAX / 2,
                FocusPane::Links => {
                    if !app.links.is_empty() {
                        app.selected_link = app.links.len() - 1;
                    }
                }
            },
            Key::Up | Key::Char('k') => match app.focus {
                FocusPane::Content => app.scroll_y = app.scroll_y.saturating_sub(1),
                FocusPane::Links => {
                    if app.selected_link > 0 {
                        app.selected_link -= 1;
                    }
                }
            },
            Key::Down | Key::Char('j') => match app.focus {
                FocusPane::Content => app.scroll_y += 1,
                FocusPane::Links => {
                    if app.selected_link + 1 < app.links.len() {
                        app.selected_link += 1;
                    }
                }
            },
            Key::PageUp => {
                app.scroll_y = app.scroll_y.saturating_sub(15);
            }
            Key::PageDown | Key::Char(' ') => {
                app.scroll_y += 15;
            }
            Key::Char('g') => {
                app.input_mode = InputMode::GotoUrl;
                app.input_buf.clear();
            }
            Key::Char('/') => {
                app.input_mode = InputMode::Search;
                app.input_buf.clear();
            }
            Key::Char('b') => {
                if let Some(prev) = app.history.pop() {
                    app.load_page_animated(&prev, false);
                } else {
                    app.status_msg = "Brak wcześniejszej strony w historii".to_string();
                }
            }
            Key::Char('r') => {
                let curr = app.current_url.clone();
                if curr != "grst://stats" {
                    app.load_page_animated(&curr, false);
                }
            }
            Key::Char('s') => {
                app.load_stats();
            }
            Key::Char(d) if d.is_ascii_digit() => {
                app.num_buf.push(d);
            }
            Key::Backspace => {
                app.num_buf.pop();
            }
            Key::Esc => {
                app.num_buf.clear();
            }
            Key::Enter => {
                if !app.num_buf.is_empty() {
                    if let Ok(num) = app.num_buf.parse::<usize>() {
                        app.num_buf.clear();
                        if let Some(href) = app.link_map.get(&num).cloned() {
                            let kind = detect_media_kind_by_url(&href);
                            let lbl = app
                                .links
                                .iter()
                                .find(|l| l.num == num)
                                .map(|l| l.label.clone())
                                .unwrap_or_else(|| format!("Link [{num}]"));
                            app.open_media_or_link(LinkItem {
                                num,
                                label: lbl,
                                url: href,
                                media_kind: kind,
                                size_bytes: None,
                            });
                        } else {
                            app.status_msg = format!("Brak linku [{num}]");
                        }
                    }
                } else if let Some(item) = app.links.get(app.selected_link).cloned() {
                    app.open_media_or_link(item);
                }
            }
            _ => {}
        }
    }
}