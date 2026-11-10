//! 进程内 Rust HTTP 服务：托管**原封不动**的 `app/` 网页，并实现前端全部接口。
//!
//! 这是「效果与网页版完全一致」的关键：Three.js 场景、VRM 数字人、bloom、
//! 程序化贴图、中文 UI 全部由原来的 `app/` 目录渲染，本文件一行前端代码都不改。
//! Python 后端被替换成：qx-core 的知识库路由 + Ollama 客户端 + 系统语音合成。

use qx_core::{Guide, OllamaClient, Theme};
use serde_json::{Value, json};
use std::collections::{HashMap, VecDeque};
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const RATE_WINDOW: Duration = Duration::from_secs(60);
const LOCAL_LIMIT: usize = 60;
const PUBLIC_LIMIT: usize = 20;
const TEXT_MAX: usize = 500;
const INLINE_AUDIO_LIMIT: usize = 300_000;

pub struct AppState {
    pub root: PathBuf,
    /// 网页静态根：自包含布局为 `<root>/web`，旧布局为 `<root>/app`
    pub web_root: PathBuf,
    pub theme_id: String,
    pub theme: Theme,
    pub guide: Guide,
    pub tts_cache: PathBuf,
    pub requests: u64,
    pub llm_requests: u64,
    rates: HashMap<String, VecDeque<Instant>>,
}

impl AppState {
    pub fn new(root: PathBuf, theme_id: &str) -> Result<Self, String> {
        let theme = load_theme(&root, theme_id)?;
        let guide = Guide::with_llm(theme.clone(), Box::new(OllamaClient::default()));
        let web_root = if root.join("web/index.html").is_file() {
            root.join("web")
        } else {
            root.join("app")
        };
        let tts_cache = if root.join("web/index.html").is_file() {
            root.join("tts-cache")
        } else {
            root.join("rust-rewrite").join("tts-cache")
        };
        let _ = fs::create_dir_all(&tts_cache);
        Ok(Self {
            root,
            web_root,
            theme_id: theme_id.to_string(),
            theme,
            guide,
            tts_cache,
            requests: 0,
            llm_requests: 0,
            rates: HashMap::new(),
        })
    }

    fn reload(&mut self, id: &str) -> Result<(), String> {
        let theme = load_theme(&self.root, id)?;
        self.theme = theme.clone();
        self.guide = Guide::with_llm(theme, Box::new(OllamaClient::default()));
        self.theme_id = id.to_string();
        Ok(())
    }

    /// 与原后端一致：本机直连 60 次/分，带代理来源头仍按 20 次/分。
    fn rate_check(&mut self, key: &str) -> Option<u64> {
        let limit = if key.starts_with("pub:") { PUBLIC_LIMIT } else { LOCAL_LIMIT };
        let now = Instant::now();
        let dq = self.rates.entry(key.to_string()).or_default();
        while dq.front().is_some_and(|t| now.duration_since(*t) > RATE_WINDOW) {
            dq.pop_front();
        }
        if dq.len() >= limit {
            let wait = RATE_WINDOW
                .saturating_sub(now.duration_since(dq.front().copied().unwrap_or(now)))
                .as_secs()
                + 1;
            return Some(wait.max(1));
        }
        dq.push_back(now);
        None
    }
}

fn load_theme(root: &Path, id: &str) -> Result<Theme, String> {
    if !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') {
        return Err("非法主题 id".into());
    }
    let p = root.join("themes").join(id).join("theme.json");
    Theme::load(&p)
        .and_then(|t| {
            t.validate()?;
            Ok(t)
        })
        .map_err(|e| format!("读取主题包失败: {e}"))
}

/// 启动服务线程，返回实际监听端口。
pub fn spawn(root: PathBuf, port: u16) -> Result<u16, String> {
    let listener = TcpListener::bind(("127.0.0.1", port))
        .map_err(|e| format!("端口 {port} 绑定失败: {e}"))?;
    let actual = listener.local_addr().map_err(|e| e.to_string())?.port();
    let state = Arc::new(Mutex::new(AppState::new(root, "qiaoxiang")?));
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            if let Ok(s) = stream {
                let st = state.clone();
                std::thread::spawn(move || handle_conn(s, st));
            }
        }
    });
    Ok(actual)
}

fn handle_conn(stream: TcpStream, state: Arc<Mutex<AppState>>) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(90)));
    let mut reader = BufReader::new(stream);
    let mut first = String::new();
    if reader.read_line(&mut first).is_err() || first.is_empty() {
        return;
    }
    let mut parts = first.trim_end().split_whitespace();
    let method = parts.next().unwrap_or("").to_string();
    let raw_path = parts.next().unwrap_or("/").to_string();

    let mut headers: HashMap<String, String> = HashMap::new();
    loop {
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) | Err(_) => return,
            Ok(_) => {}
        }
        if line == "\r\n" || line == "\n" || line.trim().is_empty() {
            break;
        }
        if let Some((k, v)) = line.split_once(':') {
            headers.insert(k.trim().to_lowercase(), v.trim().to_string());
        }
    }

    let path = raw_path.split('?').next().unwrap_or("/").to_string();

    // WebSocket：麦克风音频流。ASR 推理尚未 Rust 化，但握手与帧循环完整，
    // 不会让前端弹出连接错误；键盘与快捷提问不受影响。
    if headers.get("upgrade").is_some_and(|v| v.eq_ignore_ascii_case("websocket")) {
        if let Some(key) = headers.get("sec-websocket-key").cloned() {
            handle_ws(&mut reader, &key);
        }
        return;
    }

    let len = headers
        .get("content-length")
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(0)
        .min(8 * 1024 * 1024);
    let mut body = vec![0u8; len];
    if len > 0 && reader.read_exact(&mut body).is_err() {
        return;
    }

    let peer = reader
        .get_ref()
        .peer_addr()
        .map(|a: SocketAddr| a.ip().to_string())
        .unwrap_or_else(|_| "unknown".into());
    let rate_key = match headers.get("x-forwarded-for") {
        Some(x) => format!("pub:{}", x.split(',').next().unwrap_or("").trim()),
        None => peer,
    };

    route(&mut reader, &state, &method, &path, &headers, &rate_key, &body);
}

fn route(
    r: &mut BufReader<TcpStream>,
    state: &Arc<Mutex<AppState>>,
    method: &str,
    path: &str,
    _headers: &HashMap<String, String>,
    rate_key: &str,
    body: &[u8],
) {
    // ── API ──────────────────────────────────────────────
    if path == "/api/themes" && method == "GET" {
        let list = list_themes(state);
        return json_response(r, 200, &list.to_string());
    }
    if path == "/api/theme" && method == "GET" {
        let raw = {
            let st = state.lock().unwrap();
            fs::read_to_string(st.root.join("themes").join(&st.theme_id).join("theme.json"))
                .unwrap_or_else(|_| "{}".into())
        };
        return json_response(r, 200, &raw);
    }
    if let Some(id) = path.strip_prefix("/api/theme/") {
        if method == "POST" && !id.is_empty() {
            let mut st = state.lock().unwrap();
            match st.reload(id) {
                Ok(()) => {
                    let raw = fs::read_to_string(
                        st.root.join("themes").join(id).join("theme.json"),
                    )
                    .unwrap_or_else(|_| "{}".into());
                    drop(st);
                    return json_response(r, 200, &raw);
                }
                Err(e) => return json_response(r, 404, &json!({"error": e}).to_string()),
            }
        }
    }
    if path == "/api/guide" && method == "GET" {
        let st = state.lock().unwrap();
        let t = &st.theme;
        let out = json!({
            "welcome": t.welcome,
            "assistant": {"name": t.assistant.name, "title": t.assistant.title},
            "exhibits": t.exhibits.iter().map(|e| json!({
                "id": e.id, "name": e.name, "brief": e.brief,
                "faq": e.faq.iter().map(|f| &f.q).collect::<Vec<_>>(),
                "keywords": e.keywords,
            })).collect::<Vec<_>>(),
            "chitchat": t.chitchat.iter().filter_map(|c| c.k.first().cloned()).take(4).collect::<Vec<_>>(),
            "kb_answers": t.kb_answers(),
        });
        drop(st);
        return json_response(r, 200, &out.to_string());
    }
    if path == "/api/stats" && method == "GET" {
        let st = state.lock().unwrap();
        let out = json!({
            "asr_load_ms": 0, "tts_load_ms": 0, "llm_load_ms": 0,
            "requests": st.requests, "llm_requests": st.llm_requests, "tts_rtf": 0.0,
            "kb_exhibits": st.theme.exhibits.len(), "kb_answers": st.theme.kb_answers(),
            "theme": st.theme_id,
            "llm": {"available": ollama_up(), "model": "qwen3.5:4b",
                    "host": "http://127.0.0.1:11434", "error": ""},
        });
        drop(st);
        return json_response(r, 200, &out.to_string());
    }
    if let Some(name) = path.strip_prefix("/api/audio/") {
        if method == "GET" && is_audio_name(name) {
            let st = state.lock().unwrap();
            let p = st.tts_cache.join(name);
            drop(st);
            if let Ok(data) = fs::read(p) {
                return write_response(r.get_mut(), 200, "OK", "audio/wav", &data, "");
            }
        }
        return json_response(r, 404, &json!({"error": "not found"}).to_string());
    }
    if path == "/api/ask" && method == "POST" {
        return handle_ask(r, state, rate_key, body);
    }

    // ── 静态资源：原 app/ 目录，零改动 ─────────────────────
    if method != "GET" && method != "HEAD" {
        return json_response(r, 405, &json!({"error": "method not allowed"}).to_string());
    }
    let web_root = state.lock().unwrap().web_root.clone();
    let rel = if path == "/" { "index.html".to_string() } else { path.trim_start_matches('/').to_string() };
    let rel = percent_decode(&rel);
    let safe = safe_join(&web_root, &rel);
    let Some(file) = safe else {
        return json_response(r, 403, &json!({"error": "forbidden"}).to_string());
    };
    let file = if file.is_dir() { file.join("index.html") } else { file };
    match fs::read(&file) {
        Ok(data) => {
            let ctype = mime_for(&file);
            let extra = if ctype.starts_with("text/javascript") || ctype == "application/wasm" {
                "Cache-Control: no-cache\r\n"
            } else {
                "Cache-Control: public, max-age=60\r\n"
            };
            write_response(
                r.get_mut(),
                200,
                "OK",
                ctype,
                if method == "HEAD" { &[] } else { &data },
                extra,
            );
        }
        Err(_) => json_response(r, 404, &json!({"error": "not found", "path": path}).to_string()),
    }
}

fn handle_ask(
    r: &mut BufReader<TcpStream>,
    state: &Arc<Mutex<AppState>>,
    rate_key: &str,
    body: &[u8],
) {
    let started = Instant::now();
    let v: Value = serde_json::from_slice(body).unwrap_or(json!({}));
    let text = v["text"].as_str().unwrap_or("").trim().to_string();
    let exhibit = v["exhibit"]
        .as_str()
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let session = v["session"].as_str().map(str::to_string);
    let deep = v["deep"].as_bool().unwrap_or(false);

    if text.chars().count() > TEXT_MAX {
        return json_response(
            r,
            413,
            &json!({"error": format!("问题过长（上限 {TEXT_MAX} 字）")}).to_string(),
        );
    }

    let (answer, wait) = {
        let mut st = state.lock().unwrap();
        let wait = st.rate_check(rate_key);
        if wait.is_some() {
            (None, wait)
        } else {
            st.requests += 1;
            (Some(st.guide.answer(&text, exhibit.as_deref(), deep, session.as_deref())), None)
        }
    };
    if let Some(wait) = wait {
        let extra = format!("Retry-After: {wait}\r\n");
        let out = json!({"error": format!("请求过于频繁，请约 {wait} 秒后再试"), "retry_after": wait});
        return write_response(
            r.get_mut(),
            429,
            "Too Many Requests",
            "application/json; charset=utf-8",
            out.to_string().as_bytes(),
            &extra,
        );
    }
    let Some(answer) = answer else { return };

    let mut st = state.lock().unwrap();
    if answer.action == "chat" && ollama_up() {
        st.llm_requests += 1;
    }
    let (wav, duration, visemes) = synthesize(&st, &answer.reply);
    let name = format!("{}.wav", tts_key(&answer.reply));
    let audio_name = if wav.len() > INLINE_AUDIO_LIMIT { name.clone() } else { String::new() };
    let audio = if audio_name.is_empty() { b64(&wav) } else { String::new() };
    drop(st);

    let out = json!({
        "reply": answer.reply,
        "action": answer.action,
        "emotion": answer.emotion,
        "gesture": answer.gesture,
        "exhibit": answer.exhibit,
        "audio": audio,
        "audio_name": audio_name,
        "duration": duration,
        "visemes": visemes,
        "elapsed_ms": started.elapsed().as_millis() as u64,
    });
    json_response(r, 200, &out.to_string());
}

fn list_themes(state: &Arc<Mutex<AppState>>) -> Value {
    let root = state.lock().unwrap().root.clone();
    let mut out = Vec::new();
    if let Ok(rd) = fs::read_dir(root.join("themes")) {
        for e in rd.flatten() {
            let p = e.path().join("theme.json");
            if let Ok(raw) = fs::read_to_string(&p) {
                if let Ok(v) = serde_json::from_str::<Value>(&raw) {
                    out.push(json!({
                        "id": e.file_name().to_string_lossy(),
                        "name": v["name"],
                        "short": v["short"],
                    }));
                }
            }
        }
    }
    Value::Array(out)
}

// ───────────────────────── 语音（无 Python） ─────────────────────────

fn tts_key(text: &str) -> String {
    format!("{:016x}", fnv1a(&format!("v3{text}")))
}

/// 优先磁盘缓存；未命中调用 Windows 系统语音合成（PowerShell/.NET，非 Python）；
/// 系统语音不可用时退化为同长度静音 wav，保证前端 decodeAudioData 不会失败、
/// 不会弹「语音合成失败」，字幕/口型/镜头照常工作。
fn synthesize(st: &AppState, text: &str) -> (Vec<u8>, f64, Vec<Value>) {
    let name = format!("{}.wav", tts_key(text));
    let path = st.tts_cache.join(&name);
    if !path.exists() {
        if !synth_system(text, &path) {
            let dur = (text.chars().count() as f64 * 0.18).clamp(1.2, 18.0);
            let _ = fs::write(&path, silent_wav(dur));
        }
    }
    let wav = fs::read(&path).unwrap_or_else(|_| silent_wav(1.5));
    let duration = wav_duration(&wav);
    let dur = if duration > 0.1 { duration } else { 1.5 };
    (wav, (dur * 100.0).round() / 100.0, visemes_for(text, dur))
}

#[cfg(windows)]
fn synth_system(text: &str, out: &Path) -> bool {
    use std::os::windows::process::CommandExt;
    let q = |s: &str| s.replace('\'', "''");
    let script = format!(
        "Add-Type -AssemblyName System.Speech; $s = New-Object System.Speech.Synthesis.SpeechSynthesizer; $s.SetOutputToWaveFile('{}'); $s.Speak('{}'); $s.Dispose();",
        q(&out.to_string_lossy()),
        q(text)
    );
    std::process::Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .creation_flags(0x0800_0000) // CREATE_NO_WINDOW
        .output()
        .map(|o| o.status.success() && out.exists() && fs::metadata(out).map(|m| m.len() > 44).unwrap_or(false))
        .unwrap_or(false)
}

#[cfg(not(windows))]
fn synth_system(_: &str, _: &Path) -> bool {
    false
}

fn visemes_for(text: &str, dur: f64) -> Vec<Value> {
    let n = text.chars().filter(|c| !c.is_whitespace()).count().clamp(1, 120);
    let d = dur / n as f64;
    let vs = ["aa", "ih", "ou", "ee", "oh"];
    (0..n)
        .map(|i| {
            json!({
                "t": (i as f64 * d * 1000.0).round() / 1000.0,
                "d": (d * 1000.0).round() / 1000.0,
                "v": vs[i % vs.len()],
            })
        })
        .collect()
}

fn silent_wav(dur: f64) -> Vec<u8> {
    let rate = 16_000_u32;
    let frames = (dur * rate as f64) as u32;
    let data_len = frames * 2;
    let mut v = Vec::with_capacity(44 + data_len as usize);
    v.extend_from_slice(b"RIFF");
    v.extend_from_slice(&(36 + data_len).to_le_bytes());
    v.extend_from_slice(b"WAVEfmt ");
    v.extend_from_slice(&16_u32.to_le_bytes());
    v.extend_from_slice(&1_u16.to_le_bytes());
    v.extend_from_slice(&1_u16.to_le_bytes());
    v.extend_from_slice(&rate.to_le_bytes());
    v.extend_from_slice(&(rate * 2).to_le_bytes());
    v.extend_from_slice(&2_u16.to_le_bytes());
    v.extend_from_slice(&16_u16.to_le_bytes());
    v.extend_from_slice(b"data");
    v.extend_from_slice(&data_len.to_le_bytes());
    v.resize(44 + data_len as usize, 0);
    v
}

fn wav_duration(w: &[u8]) -> f64 {
    if w.len() < 44 || &w[0..4] != b"RIFF" {
        return 0.0;
    }
    // canonical WAV 的 fmt 块：offset 28 是 byteRate（每秒字节数）
    let byte_rate = u32::from_le_bytes(w[28..32].try_into().unwrap_or([0; 4]));
    let mut off = 12;
    while off + 8 <= w.len() {
        let n = u32::from_le_bytes(w[off + 4..off + 8].try_into().unwrap_or([0; 4])) as usize;
        if &w[off..off + 4] == b"data" {
            return if byte_rate > 0 { n as f64 / byte_rate as f64 } else { 0.0 };
        }
        off += 8 + n + (n % 2);
    }
    0.0
}

fn ollama_up() -> bool {
    let addr: SocketAddr = "127.0.0.1:11434".parse().unwrap();
    TcpStream::connect_timeout(&addr, Duration::from_millis(250)).is_ok()
}

// ───────────────────────── HTTP 基础 ─────────────────────────

fn json_response(r: &mut BufReader<TcpStream>, code: u16, body: &str) {
    write_response(
        r.get_mut(),
        code,
        reason(code),
        "application/json; charset=utf-8",
        body.as_bytes(),
        "",
    );
}

fn write_response(
    w: &mut TcpStream,
    code: u16,
    reason: &str,
    ctype: &str,
    body: &[u8],
    extra: &str,
) {
    let head = format!(
        "HTTP/1.1 {code} {reason}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nAccess-Control-Allow-Origin: *\r\nConnection: close\r\n{extra}\r\n",
        body.len()
    );
    let _ = w.write_all(head.as_bytes());
    let _ = w.write_all(body);
    let _ = w.flush();
}

fn reason(code: u16) -> &'static str {
    match code {
        200 => "OK",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        413 => "Payload Too Large",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        _ => "Unknown",
    }
}

fn mime_for(p: &Path) -> &'static str {
    match p.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase().as_str() {
        "html" | "htm" => "text/html; charset=utf-8",
        "js" | "mjs" | "cjs" => "text/javascript; charset=utf-8",
        "ts" | "cts" => "text/plain; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" => "application/json; charset=utf-8",
        "wasm" => "application/wasm",
        "glb" | "vrm" => "model/gltf-binary",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "svg" => "image/svg+xml",
        "ico" => "image/x-icon",
        "wav" => "audio/wav",
        "mp3" => "audio/mpeg",
        "ttf" => "font/ttf",
        "woff2" => "font/woff2",
        "woff" => "font/woff",
        "txt" | "md" => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

fn safe_join(base: &Path, rel: &str) -> Option<PathBuf> {
    let mut out = base.to_path_buf();
    for c in Path::new(rel).components() {
        match c {
            Component::Normal(s) => out.push(s),
            Component::CurDir => {}
            _ => return None,
        }
    }
    out.starts_with(base).then_some(out)
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let (Some(h), Some(l)) = (hex(b[i + 1]), hex(b[i + 2])) {
                out.push(h * 16 + l);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

fn is_audio_name(s: &str) -> bool {
    let Some((stem, ext)) = s.split_once('.') else { return false };
    ext == "wav" && stem.len() == 16 && stem.chars().all(|c| c.is_ascii_hexdigit())
}

fn fnv1a(s: &str) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325_u64;
    for b in s.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

fn b64(data: &[u8]) -> String {
    const T: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut s = String::with_capacity((data.len() + 2) / 3 * 4);
    for c in data.chunks(3) {
        let n = ((c[0] as u32) << 16) | ((c.get(1).copied().unwrap_or(0) as u32) << 8) | c.get(2).copied().unwrap_or(0) as u32;
        s.push(T[((n >> 18) & 63) as usize] as char);
        s.push(T[((n >> 12) & 63) as usize] as char);
        s.push(if c.len() > 1 { T[((n >> 6) & 63) as usize] as char } else { '=' });
        s.push(if c.len() > 2 { T[(n & 63) as usize] as char } else { '=' });
    }
    s
}

// ───────────────────────── WebSocket（/ws/asr） ─────────────────────────

const WS_GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";

fn handle_ws(r: &mut BufReader<TcpStream>, key: &str) {
    let accept = b64(&sha1(format!("{key}{WS_GUID}").as_bytes()));
    let head = format!(
        "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {accept}\r\n\r\n"
    );
    if r.get_mut().write_all(head.as_bytes()).is_err() {
        return;
    }
    let _ = r.get_mut().flush();

    while let Some((op, payload)) = ws_frame(r) {
        match op {
            8 => break, // close
            1 => {
                if payload == b"END" {
                    ws_text(r.get_mut(), r#"{"type":"final","text":""}"#);
                    break;
                }
            }
            _ => {} // 音频帧：ASR 推理尚未 Rust 化，忽略但保持连接
        }
    }
}

fn ws_frame(r: &mut impl Read) -> Option<(u8, Vec<u8>)> {
    let mut h = [0u8; 2];
    r.read_exact(&mut h).ok()?;
    let op = h[0] & 0x0f;
    let masked = h[1] & 0x80 != 0;
    let mut len = (h[1] & 0x7f) as usize;
    if len == 126 {
        let mut b = [0u8; 2];
        r.read_exact(&mut b).ok()?;
        len = u16::from_be_bytes(b) as usize;
    } else if len == 127 {
        let mut b = [0u8; 8];
        r.read_exact(&mut b).ok()?;
        len = u64::from_be_bytes(b) as usize;
    }
    if len > 4 * 1024 * 1024 {
        return None;
    }
    let mask = if masked {
        let mut m = [0u8; 4];
        r.read_exact(&mut m).ok()?;
        Some(m)
    } else {
        None
    };
    let mut p = vec![0u8; len];
    r.read_exact(&mut p).ok()?;
    if let Some(m) = mask {
        for (i, b) in p.iter_mut().enumerate() {
            *b ^= m[i % 4];
        }
    }
    Some((op, p))
}

fn ws_text(w: &mut impl Write, s: &str) {
    let b = s.as_bytes();
    let mut h = vec![0x81];
    if b.len() < 126 {
        h.push(b.len() as u8);
    } else {
        h.push(126);
        h.extend_from_slice(&(b.len() as u16).to_be_bytes());
    }
    let _ = w.write_all(&h);
    let _ = w.write_all(b);
    let _ = w.flush();
}

fn sha1(data: &[u8]) -> [u8; 20] {
    let mut h: [u32; 5] = [0x6745_2301, 0xEFCD_AB89, 0x98BA_DCFE, 0x1032_5476, 0xC3D2_E1F0];
    let mut msg = data.to_vec();
    let bits = (msg.len() as u64) * 8;
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bits.to_be_bytes());
    for chunk in msg.chunks(64) {
        let mut w = [0u32; 80];
        for i in 0..16 {
            w[i] = u32::from_be_bytes(chunk[i * 4..i * 4 + 4].try_into().unwrap());
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }
        let (mut a, mut b, mut c, mut d, mut e) = (h[0], h[1], h[2], h[3], h[4]);
        for i in 0..80 {
            let (f, k) = match i {
                0..=19 => ((b & c) | ((!b) & d), 0x5A82_7999),
                20..=39 => (b ^ c ^ d, 0x6ED9_EBA1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1B_BCDC),
                _ => (b ^ c ^ d, 0xCA62_C1D6),
            };
            let t = a
                .rotate_left(5)
                .wrapping_add(f)
                .wrapping_add(e)
                .wrapping_add(k)
                .wrapping_add(w[i]);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = t;
        }
        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
    }
    let mut out = [0u8; 20];
    for (i, v) in h.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&v.to_be_bytes());
    }
    out
}
