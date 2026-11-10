//! 进程内 Rust HTTP 服务：托管**原封不动**的 `app/` 网页，并实现前端全部接口。
//!
//! 这是「效果与网页版完全一致」的关键：Three.js 场景、VRM 数字人、bloom、
//! 程序化贴图、中文 UI 全部由原来的 `app/` 目录渲染，本文件一行前端代码都不改。
//! Python 后端被替换成：qx-core 的知识库路由 + Ollama 客户端 + 系统语音合成。

use crate::piper::PiperTts;
use qx_core::{Guide, LlmClient, Message, OllamaClient, Theme};
use serde_json::{Value, json};
use std::collections::{HashMap, VecDeque};
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Component, Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[cfg(windows)]
use std::os::windows::process::CommandExt;

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
    /// 问答引擎：`answer()` 只读（OllamaClient 无状态），用 Arc 跨线程共享。
    /// /api/ask 的 LLM 生成不再持有全局锁，stats / 静态资源 / 其他提问照常服务。
    pub guide: Arc<Guide>,
    /// 常驻语音合成进程（System.Speech 降级方案，见 TtsWorker）。
    pub tts: Arc<TtsWorker>,
    /// 离线神经语音（Piper VITS，原版甜美音色），首选合成引擎。
    pub piper: Arc<PiperTts>,
    pub tts_cache: PathBuf,
    pub requests: u64,
    pub llm_requests: u64,
    rates: HashMap<String, VecDeque<Instant>>,
}

impl AppState {
    pub fn new(root: PathBuf, theme_id: &str) -> Result<Self, String> {
        let theme = load_theme(&root, theme_id)?;
        let guide = Arc::new(Guide::with_llm(theme.clone(), Box::new(OllamaClient::default())));
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
        // 启动即拉起常驻合成进程并预热（Add-Type 引擎加载藏在开机阶段，约 0.5-1 s），
        // 用户第一次提问就享受 ~0.2 s 级合成，而不是 1-2 s 的进程冷启动。
        let tts = TtsWorker::start(tts_cache.clone());
        // 神经语音（首选）：ORT 会话 + huayan 模型在后台线程预加载，不拖慢开窗；
        // 预热后再空跑一句短文本，顺手得到真实的「TTS 实时率」指标。
        let piper = Arc::new(PiperTts::new(root.join("tts")));
        {
            let p = Arc::clone(&piper);
            std::thread::spawn(move || {
                p.preload();
                let t0 = Instant::now();
                if let Some(wav) = p.synth("欢迎光临") {
                    let d = wav_duration(&wav);
                    if d > 0.2 {
                        let rtf = (t0.elapsed().as_secs_f64() / d * 100.0) as u64;
                        TTS_RTF_X100.store(rtf.max(1), Ordering::Relaxed);
                    }
                }
            });
        }
        Ok(Self {
            root,
            web_root,
            theme_id: theme_id.to_string(),
            theme,
            guide,
            tts,
            piper,
            tts_cache,
            requests: 0,
            llm_requests: 0,
            rates: HashMap::new(),
        })
    }

    fn reload(&mut self, id: &str) -> Result<(), String> {
        let theme = load_theme(&self.root, id)?;
        self.theme = theme.clone();
        self.guide = Arc::new(Guide::with_llm(theme, Box::new(OllamaClient::default())));
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
    // 启动预热：把 qwen3.5:4b 提前加载进显存（配合 keep_alive=-1 常驻），
    // 用户首次提问不再支付数秒~十几秒的模型冷加载；Ollama 不在时静默跳过。
    std::thread::spawn(|| {
        if ollama_up() {
            let c = OllamaClient::default();
            let _ = c.chat(&[Message::user("（启动预热，请忽略）")], 0.0, 1);
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
            // 首选神经语音就绪耗时；未就绪时退回系统语音的就绪耗时
            "asr_load_ms": 0, "tts_load_ms": st.piper.load_ms().max(st.tts.load_ms()),
            "llm_load_ms": 0,
            // 主机版未接入离线识别：前端据此隐藏「离线语音识别」自检行
            "asr_available": false,
            "requests": st.requests, "llm_requests": st.llm_requests,
            "tts_rtf": TTS_RTF_X100.load(Ordering::Relaxed) as f64 / 100.0,
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

    // 只在取限流/计数器/共享句柄时短暂持锁；LLM 生成与语音合成全程无锁，
    // 否则一次慢提问会把 stats 轮询、静态资源、其他请求全部卡在锁上，
    // 用户看到的就是「整个界面跟着一起卡」。
    let (guide, tts, piper, tts_cache, wait) = {
        let mut st = state.lock().unwrap();
        let wait = st.rate_check(rate_key);
        if wait.is_some() {
            (None, None, None, None, wait)
        } else {
            st.requests += 1;
            (
                Some(Arc::clone(&st.guide)),
                Some(Arc::clone(&st.tts)),
                Some(Arc::clone(&st.piper)),
                Some(st.tts_cache.clone()),
                None,
            )
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
    let Some(guide) = guide else { return };
    let Some(tts) = tts else { return };
    let Some(piper) = piper else { return };
    let Some(tts_cache) = tts_cache else { return };

    let answer = guide.answer(&text, exhibit.as_deref(), deep, session.as_deref());
    if answer.action == "chat" && ollama_up() {
        state.lock().unwrap().llm_requests += 1;
    }
    let (wav, duration, visemes) = synthesize(&piper, &tts, &tts_cache, &answer.reply);
    let name = format!("{}.wav", tts_key(&answer.reply));
    let audio_name = if wav.len() > INLINE_AUDIO_LIMIT { name.clone() } else { String::new() };
    let audio = if audio_name.is_empty() { b64(&wav) } else { String::new() };

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
    // v8：讲解员换成 Q 版吉祥物，配套语音改为标准普通话卡通活泼女声
    // （zh-CN-XiaoyiNeural）。音色变化必须升版本让旧缓存失效，否则现场会
    // 继续命中 v7 的成熟播报女声，换了模型却听不出变化。
    format!("{:016x}", fnv1a(&format!("v8{text}")))
}

/// 最近一次神经语音合成的实时率（合成耗时 / 音频时长，越小越快），
/// 以百分之一为单位存整型，供 /api/stats 展示「TTS 实时率」。
static TTS_RTF_X100: AtomicU64 = AtomicU64::new(0);

/// 优先磁盘缓存；未命中按三级合成：Piper 神经语音（首选）→ 常驻系统语音进程
/// → 一次性 PowerShell；系统语音彻底不可用时写入同长度静音 wav，保证前端
/// decodeAudioData 不失败、不弹「语音合成失败」，字幕/口型/镜头照常工作。
fn synthesize(
    piper: &PiperTts,
    tts: &TtsWorker,
    tts_cache: &Path,
    text: &str,
) -> (Vec<u8>, f64, Vec<Value>) {
    let hex = tts_key(text);
    let name = format!("{hex}.wav");
    let path = tts_cache.join(&name);
    if !path.exists() {
        let t0 = Instant::now();
        if let Some(wav) = piper.synth(text) {
            let _ = fs::write(&path, &wav);
            let dur = wav_duration(&wav);
            if dur > 0.2 {
                let rtf = (t0.elapsed().as_secs_f64() / dur * 100.0) as u64;
                TTS_RTF_X100.store(rtf.max(1), Ordering::Relaxed);
            }
        } else if !tts.synth(&hex, text) && !synth_system(text, &path) {
            let dur = (text.chars().count() as f64 * 0.18).clamp(1.2, 18.0);
            let _ = fs::write(&path, silent_wav(dur));
        }
    }
    let wav = fs::read(&path).unwrap_or_else(|_| silent_wav(1.5));
    let duration = wav_duration(&wav);
    let dur = if duration > 0.1 { duration } else { 1.5 };
    (wav, (dur * 100.0).round() / 100.0, visemes_for(text, dur))
}

// ───────────────────── 常驻语音合成进程 ─────────────────────
//
// 每次请求冷启动 PowerShell + Add-Type System.Speech 要 1-2 s；实测常驻
// 引擎合成 40 字只需 ~140 ms。这里在应用启动时就拉起一个合成进程：
// stdin 收 ASCII 任务号、stdout 回执，中文文本经 UTF-8 的 .txt 落盘传递，
// 管道里只有 ASCII，彻底绕开控制台编码坑。进程意外死亡时下次调用自动
// 重新拉起；彻底失败再退回一次性进程方案。

pub struct TtsWorker {
    cache_dir: PathBuf,
    inner: Mutex<Option<Worker>>,
    load_ms: AtomicU64,
}

struct Worker {
    child: Child,
    stdin: ChildStdin,
    rx: mpsc::Receiver<String>,
}

impl TtsWorker {
    /// 拉起常驻进程并用 PING 预热（等待 Add-Type 完成），记录真实就绪耗时。
    pub fn start(cache_dir: PathBuf) -> Arc<TtsWorker> {
        let w = Arc::new(TtsWorker {
            cache_dir,
            inner: Mutex::new(None),
            load_ms: AtomicU64::new(0),
        });
        let t0 = Instant::now();
        if w.ping() {
            w.load_ms.store(t0.elapsed().as_millis() as u64, Ordering::Relaxed);
        }
        w
    }

    /// 自检面板用：常驻引擎就绪耗时（毫秒）；0 表示尚未就绪。
    pub fn load_ms(&self) -> u64 {
        self.load_ms.load(Ordering::Relaxed)
    }

    fn launch(&self) -> Option<Worker> {
        // 纯 ASCII 脚本；缓存目录经环境变量传入（进程环境是 UTF-16，无编码风险）。
        // 任务号是 ASCII hex，中文正文从 <hex>.txt（UTF-8）读取。
        let script = concat!(
            "$ErrorActionPreference='Stop';",
            "[Console]::OutputEncoding=[Text.Encoding]::ASCII;",
            "Add-Type -AssemblyName System.Speech;",
            "$s=New-Object System.Speech.Synthesis.SpeechSynthesizer;",
            // Rate=3：Huihui 默认仅 ~3.4 字/秒，拖沓感明显；实测 Rate=3 达
            // 4.7 字/秒，与原版 Piper 的自然讲解节奏一致。
            "$s.Rate=3;",
            "$dir=$env:QX_TTS_DIR;",
            "while($true){",
            "$line=[Console]::In.ReadLine();",
            "if($null -eq $line){break};",
            "$line=$line.Trim();",
            "if($line -eq ''){continue};",
            "if($line -eq 'PING'){[Console]::Out.WriteLine('OK PING');[Console]::Out.Flush();continue};",
            "try{",
            "$t=[IO.File]::ReadAllText((Join-Path $dir ($line+'.txt')),[Text.Encoding]::UTF8);",
            "$s.SetOutputToWaveFile((Join-Path $dir ($line+'.wav')));",
            "$s.Speak($t);",
            "$s.SetOutputToNull();",
            "[Console]::Out.WriteLine('OK '+$line)",
            "}catch{[Console]::Out.WriteLine('ERR '+$line)};",
            "[Console]::Out.Flush()",
            "}"
        );
        let mut child = Command::new("powershell")
            .args(["-NoProfile", "-NonInteractive", "-Command", script])
            .env("QX_TTS_DIR", &self.cache_dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .creation_flags(0x0800_0000) // CREATE_NO_WINDOW
            .spawn()
            .ok()?;
        let stdout = child.stdout.take()?;
        let stdin = child.stdin.take()?;
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let reader = BufReader::new(stdout);
            for line in reader.lines().map_while(Result::ok) {
                if tx.send(line).is_err() {
                    break;
                }
            }
            let _ = tx.send(String::new()); // EOF 哨兵，让等待方立即失败而不是干等超时
        });
        Some(Worker { child, stdin, rx })
    }

    fn kill(guard: &mut Option<Worker>) {
        if let Some(w) = guard.as_mut() {
            let _ = w.child.kill();
        }
        *guard = None;
    }

    /// PING 预热：确认引擎可用（顺带完成 Add-Type）。
    fn ping(&self) -> bool {
        let mut guard = self.inner.lock().unwrap();
        self.roundtrip(&mut guard, "PING") == Some("OK PING".into())
    }

    /// 发送任务并等待回执；持有锁直到本次合成完成（音频引擎本就是单通道）。
    fn roundtrip(&self, guard: &mut Option<Worker>, task: &str) -> Option<String> {
        for _ in 0..2 {
            if guard.is_none() {
                *guard = self.launch();
            }
            let Some(w) = guard.as_mut() else { return None };
            if w.stdin
                .write_all(task.as_bytes())
                .and_then(|_| w.stdin.write_all(b"\n"))
                .and_then(|_| w.stdin.flush())
                .is_err()
            {
                Self::kill(guard);
                continue;
            }
            match w.rx.recv_timeout(Duration::from_secs(30)) {
                Ok(s) if !s.is_empty() => return Some(s),
                _ => Self::kill(guard), // EOF 哨兵或超时 → 重拉一次
            }
        }
        None
    }

    /// 合成 `<hex>.txt`（UTF-8 正文）→ `<hex>.wav`；成功返回 true。
    pub fn synth(&self, hex: &str, text: &str) -> bool {
        let txt = self.cache_dir.join(format!("{hex}.txt"));
        if fs::write(&txt, text).is_err() {
            return false;
        }
        let mut guard = self.inner.lock().unwrap();
        let reply = self.roundtrip(&mut guard, hex);
        drop(guard);
        let _ = fs::remove_file(&txt);
        reply
            .is_some_and(|s| s == format!("OK {hex}"))
            && fs::metadata(self.cache_dir.join(format!("{hex}.wav")))
                .map(|m| m.len() > 44)
                .unwrap_or(false)
    }
}

#[cfg(windows)]
fn synth_system(text: &str, out: &Path) -> bool {
    use std::os::windows::process::CommandExt;
    let q = |s: &str| s.replace('\'', "''");
    let script = format!(
        "Add-Type -AssemblyName System.Speech; $s = New-Object System.Speech.Synthesis.SpeechSynthesizer; $s.Rate = 3; $s.SetOutputToWaveFile('{}'); $s.Speak('{}'); $s.Dispose();",
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

// ───────────────────────── 冒烟测试（仅测试编译） ─────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// 端到端冒烟：Piper 神经语音预热 → 问答 → 合成 → 校验 wav 与缓存。
    /// 顺带把 v8 键的 KB 语音预热进 tts-cache，正式运行首次提问即命中缓存。
    #[test]
    fn tts_worker_ask_and_synth_smoke() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../");
        let state = Arc::new(Mutex::new(AppState::new(root, "qiaoxiang").expect("AppState")));
        let (piper, tts, tts_cache, guide) = {
            let st = state.lock().unwrap();
            (
                Arc::clone(&st.piper),
                Arc::clone(&st.tts),
                st.tts_cache.clone(),
                Arc::clone(&st.guide),
            )
        };
        piper.preload();
        assert!(piper.load_ms() > 0, "Piper 神经语音应在预热后就绪");

        let answer = guide.answer("介绍开平碉楼", None, false, None);
        assert!(!answer.reply.is_empty(), "知识库应命中碉楼条目");
        println!("reply({}字): {}", answer.reply.chars().count(), answer.reply);

        let hex = tts_key(&answer.reply);
        let (wav, dur, visemes) = synthesize(&piper, &tts, &tts_cache, &answer.reply);
        assert!(wav.len() > 44, "合成产物应是有效 wav");
        assert!(dur > 0.5, "时长应来自真实 wav 帧数，实测 {dur}s");
        assert!(!visemes.is_empty(), "口型轨道应与文本等长");
        let path = tts_cache.join(format!("{hex}.wav"));
        assert!(path.is_file(), "v8 缓存文件应已落盘");

        // 神经语音口径：约 5 字/秒，不应出现系统语音那种拖沓
        let rate = answer.reply.chars().count() as f64 / dur;
        assert!(rate > 3.5, "语速应接近真人讲解（>3.5 字/秒），实测 {rate:.1}");

        // 二次合成同文本：直接命中磁盘缓存
        let (wav2, dur2, _) = synthesize(&piper, &tts, &tts_cache, &answer.reply);
        assert_eq!(wav.len(), wav2.len());
        assert!((dur - dur2).abs() < 1e-9);
    }

    /// 赛前预生成的标准普通话语音按 tts-cache 键命名；若 tts_key 版本或
    /// 主题文案变动而没重跑预生成，现场会静默回退到 Piper，音质骤降。
    /// 未启用预生成（目录为空）时自动跳过，保持纯离线部署可用。
    #[test]
    fn prebuilt_voice_keys_match_runtime() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../");
        let dir = root.join("prebuilt-voices");
        let Ok(rd) = std::fs::read_dir(&dir) else { return };
        if !rd.flatten().any(|e| e.path().extension().is_some_and(|x| x == "wav")) {
            return;
        }
        let manifest = root.join("voice_manifest.json");
        let Ok(raw) = std::fs::read_to_string(&manifest) else { return };
        let list: serde_json::Value = serde_json::from_str(&raw).expect("清单应为合法 JSON");
        for item in list.as_array().expect("清单应为数组") {
            let text = item["text"].as_str().expect("清单条目缺少 text");
            let name = format!("{}.wav", tts_key(text));
            assert!(
                dir.join(&name).is_file(),
                "缓存键不一致：{} 期望 {name}，请重跑 tools/prebuild_voices.py",
                item["tag"]
            );
        }
        println!("预生成语音 {} 条缓存键全部匹配", list.as_array().unwrap().len());
    }

    /// 人机互动环节：每道题的追问必须命中知识库，且对应答案的语音必须已赛前
    /// 预生成。否则现场会静默回退到 Piper（普通话不标准），演示效果打折。
    /// 改题库 / 改讲解词 / 升缓存键版本时，这条测试会第一时间拦下来。
    #[test]
    fn quiz_followups_hit_knowledge_base() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../");
        let theme_path = root.join("themes/qiaoxiang/theme.json");
        let raw = std::fs::read_to_string(&theme_path).expect("读取 theme.json");
        let json: serde_json::Value = serde_json::from_str(&raw).expect("theme.json 应为合法 JSON");
        let quiz = json["quiz"].as_array().expect("主题包应包含 quiz 题库");
        assert!(!quiz.is_empty(), "互动题库不应为空");

        let guide = Guide::new(Theme::load(theme_path.to_string_lossy().to_string()).expect("加载主题包"));
        let prebuilt = root.join("prebuilt-voices");
        for (i, q) in quiz.iter().enumerate() {
            let opts = q["options"].as_array().expect("题目缺少 options");
            let answer = q["answer"].as_u64().expect("题目缺少 answer") as usize;
            assert!(opts.len() >= 2 && answer < opts.len(), "第 {i} 题选项或答案越界");
            let followup = q["followup"].as_str().expect("题目缺少 followup");
            let a = guide.answer(followup, None, false, None);
            assert!(!a.reply.is_empty(), "第 {i} 题追问「{followup}」没有答案");
            assert_ne!(a.action, "chat", "第 {i} 题追问「{followup}」未命中知识库，会走大模型");
            let key = format!("{}.wav", tts_key(&a.reply));
            assert!(
                prebuilt.join(&key).is_file(),
                "第 {i} 题追问「{followup}」缺少预生成语音 {key}，请重跑 tools/prebuild_voices.py"
            );
        }
        println!("互动题库 {} 题：追问全部命中知识库且已有标准普通话语音", quiz.len());
    }
}
