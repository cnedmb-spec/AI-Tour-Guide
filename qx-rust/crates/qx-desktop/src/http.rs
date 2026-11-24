//! 进程内 Rust HTTP 服务：托管**原封不动**的 `app/` 网页，并实现前端全部接口。
//!
//! 这是「效果与网页版完全一致」的关键：Three.js 场景、VRM 数字人、bloom、
//! 程序化贴图、中文 UI 全部由原来的 `app/` 目录渲染，本文件一行前端代码都不改。
//! Python 后端被替换成：qx-core 的知识库路由 + Ollama 客户端 + 系统语音合成。

use crate::asr::{Asr, AsrEvent};
use crate::piper::PiperTts;
use qx_core::{Guide, LlamaServerClient, LlmClient, Message, NoLlm, OllamaClient, Theme};
use serde_json::{Value, json};
use std::collections::{HashMap, VecDeque};
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

const RATE_WINDOW: Duration = Duration::from_secs(60);
const LOCAL_LIMIT: usize = 60;
const PUBLIC_LIMIT: usize = 20;
const TEXT_MAX: usize = 500;
const INLINE_AUDIO_LIMIT: usize = 300_000;
// 与前端 images.js 一致：最多 3 张、单张 base64 上限 2.5M 字符。
// 请求体整体上限见 parse_request 的 8 MiB，压缩后一般远小于此。
const IMAGE_COUNT_MAX: usize = 3;
const IMAGE_CHARS_MAX: usize = 2_500_000;
/// 前端日志单次请求体上限；正常只有一条错误消息 + 堆栈。
const FRONTEND_LOG_MAX: usize = 16 * 1024;
const HTTP_WORKERS: usize = 8;
const HTTP_QUEUE_CAPACITY: usize = 32;

/// LLM 后端选择：决定问答里的自由回答走哪条推理通道。
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum LlmBackend {
    /// 默认：走 Ollama 兼容协议（本地或远端 Ollama / llama-server 都行）。
    Auto,
    /// 显式走 Ollama 兼容协议（/api/chat）。
    Ollama,
    /// 显式走 llama.cpp server（llama-server）OpenAI 兼容协议（/v1/chat/completions）。
    LlamaServer,
    /// 本地 GGUF 模型（需 `--features local-llm` 构建，并把路径传给 --model）。
    Local,
    /// 完全关闭 LLM，自由回答回落到兜底话术（演示保底）。
    None,
}

impl Default for LlmBackend {
    fn default() -> Self {
        LlmBackend::Auto
    }
}

/// 启动参数解析出的 LLM 配置，贯穿 AppState 的生命周期。
/// 不能用 derive(Default)：那会把地址与模型名初始化成空字符串，
/// 双击启动时 Ollama 客户端就会连到空地址、请求体里 model 也为空，
/// 结果所有自由问答静默退回兜底话术。默认值必须与 OllamaClient 一致。
#[derive(Clone)]
pub struct LlmConfig {
    pub backend: LlmBackend,
    /// Ollama / llama-server 兼容协议的地址（默认本机 11434，即 Ollama 标准端口）。
    pub ollama_url: String,
    /// 模型名（默认 qwen3.5:4b，保持与旧行为一致；本地 GGUF 旁路时也用作展示名）。
    pub ollama_model: String,
    /// 本地 GGUF 模型文件路径（Local 后端使用）。
    pub local_model: Option<PathBuf>,
    /// 多模态投影器 GGUF：内置 Qwen2.5-VL 用它看图（外置纯文本 GGUF 时为 None）。
    pub local_mmproj: Option<PathBuf>,
    /// 是否使用 exe 内嵌模型；为真时不走 Ollama 视觉旁路，且 stats 显示内置模型名。
    pub embedded_model: bool,
}

impl Default for LlmConfig {
    fn default() -> Self {
        let d = OllamaClient::default();
        Self {
            backend: LlmBackend::Auto,
            ollama_url: d.host,
            ollama_model: d.model,
            local_model: None,
            local_mmproj: None,
            embedded_model: false,
        }
    }
}

/// 根据配置构造一个 LLM 客户端，可跨线程共享。
pub fn build_llm(cfg: &LlmConfig) -> Arc<dyn LlmClient> {
    match cfg.backend {
        LlmBackend::None => Arc::new(NoLlm),
        LlmBackend::Ollama | LlmBackend::Auto => {
            // 防御性兜底：即便有人手工构造了空配置，也不能让后端连到空地址。
            let d = OllamaClient::default();
            Arc::new(OllamaClient {
                host: if cfg.ollama_url.trim().is_empty() {
                    d.host
                } else {
                    cfg.ollama_url.clone()
                },
                model: if cfg.ollama_model.trim().is_empty() {
                    d.model
                } else {
                    cfg.ollama_model.clone()
                },
                timeout: Duration::from_secs(45),
            })
        }
        LlmBackend::LlamaServer => {
            let d = OllamaClient::default(); // 只借用默认地址/模型作为兜底
            Arc::new(LlamaServerClient {
                host: if cfg.ollama_url.trim().is_empty() {
                    d.host
                } else {
                    cfg.ollama_url.clone()
                },
                model: if cfg.ollama_model.trim().is_empty() {
                    d.model
                } else {
                    cfg.ollama_model.clone()
                },
                timeout: Duration::from_secs(45),
            })
        }
        LlmBackend::Local => match &cfg.local_model {
            Some(path) => {
                #[cfg(feature = "local-llm")]
                {
                    match crate::local_llm::LocalLlm::load(path, cfg.local_mmproj.as_deref()) {
                        Ok(m) => {
                            // 内置 Qwen2.5-VL 自带 mmproj，看图就在本进程完成，
                            // 不必（也不该）再经 Ollama 旁路；只有外置纯文本 GGUF
                            // 才需要把图片请求转给本机 VL 模型。
                            if cfg.embedded_model {
                                tracing::info!("内置 VL 模型已就绪：文本与看图均由本进程推理");
                                return Arc::new(m);
                            }
                            let host = if cfg.ollama_url.trim().is_empty() {
                                OllamaClient::default().host
                            } else {
                                cfg.ollama_url.clone()
                            };
                            let fb = crate::vision_route::probe_vision(&host);
                            match &fb {
                                Some(f) => tracing::info!(
                                    model = %f.model,
                                    "Local 版视觉旁路已启用：图片请求转由 Ollama 处理"
                                ),
                                None => tracing::warn!(
                                    "Local 版未找到可用的视觉模型，看图请求将按原样降级"
                                ),
                            }
                            let primary: Arc<dyn LlmClient> = Arc::new(m);
                            Arc::new(crate::vision_route::VisionRoute::new(primary, fb))
                        }
                        Err(e) => {
                            tracing::error!(error = %e, "本地模型加载失败，回退到无 LLM");
                            Arc::new(NoLlm)
                        }
                    }
                }
                #[cfg(not(feature = "local-llm"))]
                {
                    let _ = path; // 默认构建未编译本地引擎，路径仅用于提示
                    tracing::warn!(
                        "指定了 --model 但未用 `--features local-llm` 构建，本地引擎未编译；回退到无 LLM。\
                         立即自包含演示请用 llama-server 旁路：另开一个终端 `llama-server -m <模型> --port 9931`，\
                         本程序保持默认 Ollama 后端即可（它兼容 llama-server 的 /api/chat）。"
                    );
                    Arc::new(NoLlm)
                }
            }
            None => {
                tracing::warn!("后端为 Local 但未提供 --model 路径，回退到无 LLM");
                Arc::new(NoLlm)
            }
        },
    }
}

/// 供 /api/stats 展示的后端类型与模型名。
fn describe_llm(cfg: &LlmConfig) -> (String, String) {
    match cfg.backend {
        LlmBackend::None => ("none".into(), "-".into()),
        LlmBackend::Local => (
            "local".into(),
            if cfg.embedded_model {
                "内置 Qwen2.5-VL-3B".into()
            } else {
                cfg.local_model
                    .as_ref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_default()
            },
        ),
        LlmBackend::Ollama | LlmBackend::Auto => (
            "ollama".into(),
            if cfg.ollama_model.trim().is_empty() {
                OllamaClient::default().model
            } else {
                cfg.ollama_model.clone()
            },
        ),
        LlmBackend::LlamaServer => (
            "llama-server".into(),
            if cfg.ollama_model.trim().is_empty() {
                OllamaClient::default().model
            } else {
                cfg.ollama_model.clone()
            },
        ),
    }
}

pub struct AppState {
    pub root: PathBuf,
    /// 网页静态根：自包含布局为 `<root>/web`，旧布局为 `<root>/app`
    pub web_root: PathBuf,
    pub theme_id: String,
    pub theme: Theme,
    /// 问答引擎：`answer()` 只读（LLM 客户端无状态），用 Arc 跨线程共享。
    /// /api/ask 的 LLM 生成不再持有全局锁，stats / 静态资源 / 其他提问照常服务。
    pub guide: Arc<Guide>,
    /// 与 guide 共享的同一份 LLM 客户端（主题切换复用，绝不重复加载本地模型）。
    llm: Arc<dyn LlmClient>,
    /// 启动参数里的 LLM 配置（后台加载与 /api/stats 展示用）。
    llm_cfg: LlmConfig,
    /// 内置模型是否仍在后台加载；加载完成后 install_llm 置回 false。
    llm_loading: bool,
    /// 供 /api/stats 展示的后端类型与模型名。
    llm_backend: String,
    llm_model: String,
    /// 离线神经语音（Piper VITS，项目内 Rust 音素化 + ONNX Runtime，zh_CN-huayan-medium），
    /// 所有语音合成均使用此本地 ONNX 模型，不再调用云端 Edge TTS。
    pub piper: Arc<PiperTts>,
    pub tts_cache: PathBuf,
    /// 离线语音识别引擎（cpal 采集 + valle-asr/Qwen3-ASR）。不可用时 /api/stats 会如实上报。
    pub asr: Arc<Asr>,
    pub requests: u64,
    pub llm_requests: u64,
    rates: HashMap<String, VecDeque<Instant>>,
}

impl AppState {
    pub fn new(root: PathBuf, theme_id: &str, cfg: LlmConfig) -> Result<Self, String> {
        let theme = load_theme(&root, theme_id)?;
        // 内置 GGUF 加载要几秒到几十秒：先放 NoLlm 占位，HTTP/窗口先跑起来，
        // 由 spawn() 里的后台线程加载完成后 install_llm() 热替换。
        // Ollama/Auto 客户端构造是毫秒级，保持同步，行为与旧版一致。
        let defer_llm = matches!(cfg.backend, LlmBackend::Local) && cfg.local_model.is_some();
        let (llm, llm_loading) = if defer_llm {
            (Arc::new(NoLlm) as Arc<dyn LlmClient>, true)
        } else {
            (build_llm(&cfg), false)
        };
        let guide = Arc::new(Guide::with_llm(theme.clone(), Arc::clone(&llm)));
        let (llm_backend, llm_model) = describe_llm(&cfg);
        tracing::info!(
            theme = %theme_id,
            backend = %llm_backend,
            model = %llm_model,
            loading = llm_loading,
            "主题包与 LLM 后端已加载"
        );
        if llm_loading {
            tracing::info!("内置模型改为后台加载，窗口与 HTTP 服务先行就绪");
        } else if !guide.llm_available() {
            tracing::warn!("大模型不可用，自由问答将只用知识库兜底");
        }
        let web_root = if root.join("web/index.html").is_file() {
            root.join("web")
        } else {
            root.join("app")
        };
        let tts_cache = if root.join("web/index.html").is_file() {
            root.join("tts-cache")
        } else {
            root.join("qx-rust").join("tts-cache")
        };
        let _ = fs::create_dir_all(&tts_cache);
        // 启动先记录 bundle 释放出来的预生成语音：LRU 清理时绝不删除它们。
        let protected_voices = snapshot_audio_names(&tts_cache);
        prune_tts_cache(&tts_cache, &protected_voices);
        // 神经语音：ORT 会话 + huayan 模型在后台线程预加载，不拖慢开窗；
        // 预热后再空跑一句短文本，顺手得到真实的「TTS 实时率」指标。
        //
        // PiperTts 自行解析 QX_TTS_MODEL；无效路径时回退到随包模型，
        // 避免依赖开发者机器上的固定桌面路径。
        let piper = Arc::new(PiperTts::new(root.join("tts")));
        // 同步预加载 Piper 引擎，确保 AppState::new() 返回时语音已就绪，
        // 直接双击 exe 也能立即使用离线语音，不必等待后台线程。
        piper.preload();
        let t0 = Instant::now();
        if let Some(wav) = piper.synth("欢迎光临") {
            let d = wav_duration(&wav);
            if d > 0.2 {
                let rtf = (t0.elapsed().as_secs_f64() / d * 100.0) as u64;
                TTS_RTF_X100.store(rtf.max(1), Ordering::Relaxed);
            }
        }
        // ASR 也走后台加载：先读 tokenizer/配置，1.8 GB 权重在首次推理时懒加载并预热，
        // 不让首屏等模型；加载期间 available() 仍为 true，真正转写会等待引擎就绪。
        let asr = Asr::spawn_load(root.clone());
        Ok(Self {
            root,
            web_root,
            theme_id: theme_id.to_string(),
            theme,
            guide,
            llm,
            llm_cfg: cfg,
            llm_loading,
            llm_backend,
            llm_model,
            piper,
            asr,
            tts_cache,
            requests: 0,
            llm_requests: 0,
            rates: HashMap::new(),
        })
    }

    /// 主题切换：只换主题与 Guide，复用同一份 LLM 客户端。
    /// 旧实现每次 reload 都 build_llm → LocalLlm::load → Box::leak，
    /// 切一次主题就泄漏约 3 GB 模型并卡住全局锁，必须避免。
    fn reload(&mut self, id: &str) -> Result<(), String> {
        let theme = load_theme(&self.root, id)?;
        self.guide = Arc::new(Guide::with_llm(theme.clone(), Arc::clone(&self.llm)));
        self.theme = theme;
        self.theme_id = id.to_string();
        Ok(())
    }

    /// 后台模型加载完成后安装：整体替换客户端与 Guide，短暂持锁。
    fn install_llm(&mut self, llm: Arc<dyn LlmClient>, cfg: &LlmConfig) {
        let (llm_backend, llm_model) = describe_llm(cfg);
        self.guide = Arc::new(Guide::with_llm(self.theme.clone(), Arc::clone(&llm)));
        self.llm = llm;
        self.llm_backend = llm_backend;
        self.llm_model = llm_model;
        self.llm_loading = false;
        if !self.guide.llm_available() {
            tracing::warn!("大模型后台加载失败，自由问答回落到知识库兜底");
        }
    }

    /// 与原后端一致：本机直连 60 次/分，带代理来源头仍按 20 次/分。
    fn rate_check(&mut self, key: &str) -> Option<u64> {
        let limit = if key.starts_with("pub:") {
            PUBLIC_LIMIT
        } else {
            LOCAL_LIMIT
        };
        let now = Instant::now();
        let dq = self.rates.entry(key.to_string()).or_default();
        while dq
            .front()
            .is_some_and(|t| now.duration_since(*t) > RATE_WINDOW)
        {
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
    if !id
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
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
pub fn spawn(root: PathBuf, port: u16, cfg: LlmConfig) -> Result<u16, String> {
    let listener =
        TcpListener::bind(("127.0.0.1", port)).map_err(|e| format!("端口 {port} 绑定失败: {e}"))?;
    let actual = listener.local_addr().map_err(|e| e.to_string())?.port();
    let cfg_bg = cfg.clone();
    let state = Arc::new(Mutex::new(AppState::new(root, "qiaoxiang", cfg)?));
    tracing::info!(port = actual, "HTTP 服务监听已就绪");
    let defer_llm = state.lock().unwrap().llm_loading;
    // 预热用的客户端克隆（与 AppState 内同一份），避免持有 state 锁。
    let warmup_llm = Arc::clone(&state.lock().unwrap().llm);
    // 固定工作线程 + 有界队列，避免慢连接触发无上限线程增长。
    let (tx, rx) = mpsc::sync_channel::<TcpStream>(HTTP_QUEUE_CAPACITY);
    let rx = Arc::new(Mutex::new(rx));
    for _ in 0..HTTP_WORKERS {
        let rx = Arc::clone(&rx);
        let state = Arc::clone(&state);
        std::thread::spawn(move || {
            loop {
                // 只在取连接时持有接收锁；处理请求期间其他 worker 可继续取任务。
                let stream = { rx.lock().unwrap().recv() };
                match stream {
                    Ok(stream) => handle_conn(stream, Arc::clone(&state)),
                    Err(_) => break,
                }
            }
        });
    }
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { continue };
            if let Err(mpsc::TrySendError::Full(stream)) = tx.try_send(stream) {
                let _ = stream.shutdown(Shutdown::Both);
            }
        }
    });
    if defer_llm {
        // 后台装载内置 GGUF：加载完成后在短锁内整体替换 llm/guide。
        // 加载期间 /api/ask 走知识库与兜底话术，窗口和静态资源完全不受影响。
        let bg_state = Arc::clone(&state);
        std::thread::spawn(move || {
            let t0 = Instant::now();
            let llm = build_llm(&cfg_bg);
            {
                let mut st = bg_state.lock().unwrap();
                st.install_llm(Arc::clone(&llm), &cfg_bg);
            }
            tracing::info!(
                elapsed_ms = t0.elapsed().as_millis() as u64,
                available = llm.available(),
                "内置模型后台加载结束"
            );
            if llm.available() {
                if let Err(e) = llm.chat(&[Message::user("（启动预热，请忽略）")], 0.0, 1)
                {
                    tracing::debug!(error = %e, "大模型启动预热失败（首次提问会变慢，不影响功能）");
                }
            }
        });
    } else {
        // 启动预热：把模型提前加载常驻（Ollama keep_alive=-1 / 本地 GGUF 已常驻），
        // 用户首次提问不再支付数秒~十几秒的冷加载；后端不可用时静默跳过。
        std::thread::spawn(move || {
            if warmup_llm.available() {
                if let Err(e) = warmup_llm.chat(&[Message::user("（启动预热，请忽略）")], 0.0, 1)
                {
                    tracing::debug!(error = %e, "大模型启动预热失败（首次提问会变慢，不影响功能）");
                }
            }
        });
    }
    Ok(actual)
}

fn handle_conn(stream: TcpStream, state: Arc<Mutex<AppState>>) {
    let started = Instant::now();
    let _ = stream.set_read_timeout(Some(Duration::from_secs(90)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(30)));
    let _ = stream.set_nodelay(true);
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

    // WebSocket：/ws/asr 仅作控制/结果通道（END 收尾），麦克风由后端 cpal 采集，见 asr.rs。
    if headers
        .get("upgrade")
        .is_some_and(|v| v.eq_ignore_ascii_case("websocket"))
    {
        if !origin_allowed(&headers) {
            let status = json_response(
                &mut reader,
                403,
                &json!({"error": "origin not allowed"}).to_string(),
            );
            log_access(&method, &path, status, started);
            return;
        }
        if let Some(key) = headers.get("sec-websocket-key").cloned() {
            // 只 clone Arc，不把 state 锁带进可能持续 28 秒的录音循环。
            let asr = Arc::clone(&state.lock().unwrap().asr);
            handle_ws(&mut reader, &key, &asr);
        }
        tracing::info!(
            method = %method,
            path = %path,
            status = 101,
            duration_ms = started.elapsed().as_millis() as u64,
            "WebSocket 会话结束"
        );
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

    // 有副作用的接口只接受本机同源请求：任何网页都能向 127.0.0.1 发简单 POST，
    // 不校验 Origin 的话，恶意页面可以跨域触发主题重载（泄漏模型）或灌日志。
    if method == "POST" && !origin_allowed(&headers) {
        let status = json_response(
            &mut reader,
            403,
            &json!({"error": "origin not allowed"}).to_string(),
        );
        log_access(&method, &path, status, started);
        return;
    }

    let peer = reader
        .get_ref()
        .peer_addr()
        .map(|a: SocketAddr| a.ip().to_string())
        .unwrap_or_else(|_| "unknown".into());
    // x-forwarded-for 客户端可任意伪造，轮换即可绕过限流；只有显式配置了
    // 可信反代（QX_TRUST_PROXY=1）才采用，否则一律以 TCP 对端 IP 为准。
    let trust_proxy = std::env::var("QX_TRUST_PROXY").is_ok_and(|v| !v.is_empty() && v != "0");
    let rate_key = if trust_proxy {
        match headers.get("x-forwarded-for") {
            Some(x) => format!("pub:{}", x.split(',').next().unwrap_or("").trim()),
            None => peer,
        }
    } else {
        peer
    };

    let status = route(
        &mut reader,
        &state,
        &method,
        &path,
        &headers,
        &rate_key,
        &body,
    );
    log_access(&method, &path, status, started);
}

/// 同源校验：无 Origin（curl / 同源旧客户端）放行；有 Origin 则只允许
/// 127.0.0.1 / localhost 页面。`null` 等沙箱来源一律拒绝。
fn origin_allowed(headers: &HashMap<String, String>) -> bool {
    let Some(origin) = headers.get("origin") else {
        return true;
    };
    let o = origin.trim().to_ascii_lowercase();
    if o.is_empty() {
        return true;
    }
    o == "http://127.0.0.1"
        || o == "http://localhost"
        || o.starts_with("http://127.0.0.1:")
        || o.starts_with("http://localhost:")
}

/// 访问日志分级：API 正常请求记 info；静态资源记 debug（一次开页几十个请求，
/// 默认 info 级别不写文件）；4xx 记 warn、5xx 记 error，便于直接筛问题。
fn log_access(method: &str, path: &str, status: u16, started: Instant) {
    let duration_ms = started.elapsed().as_millis() as u64;
    if status >= 500 {
        tracing::error!(method = %method, path = %path, status, duration_ms, "HTTP 请求");
    } else if status >= 400 {
        tracing::warn!(method = %method, path = %path, status, duration_ms, "HTTP 请求");
    } else if path.starts_with("/api/") {
        tracing::info!(method = %method, path = %path, status, duration_ms, "HTTP 请求");
    } else {
        tracing::debug!(method = %method, path = %path, status, duration_ms, "HTTP 请求");
    }
}

fn route(
    r: &mut BufReader<TcpStream>,
    state: &Arc<Mutex<AppState>>,
    method: &str,
    path: &str,
    _headers: &HashMap<String, String>,
    rate_key: &str,
    body: &[u8],
) -> u16 {
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
                    let raw =
                        fs::read_to_string(st.root.join("themes").join(id).join("theme.json"))
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
            // 语音识别 / 合成就绪耗时（ASR 权重首次推理后再预热）
            "asr_load_ms": st.asr.load_ms(), "tts_load_ms": st.piper.load_ms(),
            "llm_load_ms": 0,
            // 离线识别已接入：前端据此启用「按住说话」并显示自检行
            "asr_available": st.asr.available(),
            // 诊断信息：模型缺失/损坏时告诉自检面板具体原因
            "asr_error": st.asr.error(),
            "requests": st.requests, "llm_requests": st.llm_requests,
            "tts_rtf": TTS_RTF_X100.load(Ordering::Relaxed) as f64 / 100.0,
            "tts_offline": st.piper.load_ms() > 0,
            // 赛前烘焙覆盖率与断网音色守卫状态，现场自检用
            "tts_cache_files": cache_audio_count(&st.tts_cache),
            "kb_exhibits": st.theme.exhibits.len(), "kb_answers": st.theme.kb_answers(),
            "theme": st.theme_id,
            "log_dir": crate::logging::dir().map(|p| p.display().to_string()).unwrap_or_default(),
            "log_file": crate::logging::latest_file().map(|p| p.display().to_string()).unwrap_or_default(),
            "llm": {"available": st.guide.llm_available(), "vision": st.guide.vision_available(),
                    "model": st.llm_model, "backend": st.llm_backend,
                    "embedded": st.llm_cfg.embedded_model,
                    "host": st.llm_cfg.ollama_url, "error": ""},
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
                let ct = if data.starts_with(b"RIFF") {
                    "audio/wav"
                } else {
                    "audio/mpeg"
                };
                return write_response(r.get_mut(), 200, "OK", ct, &data, "");
            }
        }
        return json_response(r, 404, &json!({"error": "not found"}).to_string());
    }
    if path == "/api/ask" && method == "POST" {
        return handle_ask(r, state, rate_key, body);
    }
    if path == "/api/log" && method == "POST" {
        return handle_frontend_log(r, body);
    }

    // ── 静态资源：原 app/ 目录，零改动 ─────────────────────
    if method != "GET" && method != "HEAD" {
        return json_response(r, 405, &json!({"error": "method not allowed"}).to_string());
    }
    let web_root = state.lock().unwrap().web_root.clone();
    let rel = if path == "/" {
        "index.html".to_string()
    } else {
        path.trim_start_matches('/').to_string()
    };
    let rel = percent_decode(&rel);
    let safe = safe_join(&web_root, &rel);
    let Some(file) = safe else {
        return json_response(r, 403, &json!({"error": "forbidden"}).to_string());
    };
    let file = if file.is_dir() {
        file.join("index.html")
    } else {
        file
    };
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
            )
        }
        Err(_) => json_response(
            r,
            404,
            &json!({"error": "not found", "path": path}).to_string(),
        ),
    }
}

fn handle_ask(
    r: &mut BufReader<TcpStream>,
    state: &Arc<Mutex<AppState>>,
    rate_key: &str,
    body: &[u8],
) -> u16 {
    let started = Instant::now();
    let v: Value = serde_json::from_slice(body).unwrap_or(json!({}));
    let text = v["text"].as_str().unwrap_or("").trim().to_string();
    let exhibit = v["exhibit"]
        .as_str()
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let session = v["session"].as_str().map(str::to_string);
    let deep = v["deep"].as_bool().unwrap_or(false);
    // 前端把压缩后的图片 base64 放在 images 里（迁移前 Python 版即此协议）。
    let images: Vec<String> = v["images"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|x| x.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();

    tracing::info!(
        text_chars = text.chars().count(),
        images = images.len(),
        exhibit = %exhibit.as_deref().unwrap_or(""),
        deep,
        "收到提问"
    );
    // 默认 info 不记录原文，避免把观众对话写进磁盘；排障时用 QX_LOG=debug 打开。
    tracing::debug!(text = %text, "提问原文（仅 debug 级别）");

    if text.chars().count() > TEXT_MAX {
        return json_response(
            r,
            413,
            &json!({"error": format!("问题过长（上限 {TEXT_MAX} 字）")}).to_string(),
        );
    }
    if images.len() > IMAGE_COUNT_MAX || images.iter().any(|s| s.len() > IMAGE_CHARS_MAX) {
        return json_response(
            r,
            413,
            &json!({"error": format!("图片过多或过大（最多 {IMAGE_COUNT_MAX} 张）")}).to_string(),
        );
    }

    // 只在取限流/计数器/共享句柄时短暂持锁；LLM 生成与语音合成全程无锁，
    // 否则一次慢提问会把 stats 轮询、静态资源、其他请求全部卡在锁上，
    // 用户看到的就是「整个界面跟着一起卡」。
    let (guide, piper, tts_cache, wait) = {
        let mut st = state.lock().unwrap();
        let wait = st.rate_check(rate_key);
        if wait.is_some() {
            (None, None, None, wait)
        } else {
            st.requests += 1;
            (
                Some(Arc::clone(&st.guide)),
                Some(Arc::clone(&st.piper)),
                Some(st.tts_cache.clone()),
                None,
            )
        }
    };
    if let Some(wait) = wait {
        tracing::warn!(wait_secs = wait, "请求触发限流");
        let extra = format!("Retry-After: {wait}\r\n");
        let out =
            json!({"error": format!("请求过于频繁，请约 {wait} 秒后再试"), "retry_after": wait});
        return write_response(
            r.get_mut(),
            429,
            "Too Many Requests",
            "application/json; charset=utf-8",
            out.to_string().as_bytes(),
            &extra,
        );
    }
    let (Some(guide), Some(piper), Some(tts_cache)) = (guide, piper, tts_cache) else {
        tracing::error!("问答状态不完整（内部错误）");
        return json_response(r, 500, &json!({"error": "内部状态不可用"}).to_string());
    };

    // 联网实时信息（天气 / 新闻 / 比分）：命中意图且取数成功时直接成句返回，
    // 不走大模型——这些数字不需要创作，模板更稳也不会编。取数失败（断网、
    // 接口改版、无 curl）静默落回下面原有链路，由知识库/模型照旧说「查不到」。
    if images.is_empty() {
        if let Some(reply) = crate::online::answer(&text) {
            let (wav, duration, visemes) = synthesize(&piper, &tts_cache, &reply);
            let name = format!("{}.wav", tts_key(&reply));
            let (audio_name, audio) = audio_payload(&tts_cache, &name, &wav);
            let out = json!({
                "reply": reply,
                "action": "chat",
                "emotion": "happy",
                "gesture": "idle",
                "exhibit": exhibit,
                "audio": audio,
                "audio_name": audio_name,
                "duration": duration,
                "visemes": visemes,
                "elapsed_ms": started.elapsed().as_millis() as u64,
            });
            tracing::info!(
                action = "chat",
                source = "online",
                elapsed_ms = started.elapsed().as_millis() as u64,
                "问答完成"
            );
            return json_response(r, 200, &out.to_string());
        }
    }

    let answer =
        guide.answer_with_images(&text, exhibit.as_deref(), deep, session.as_deref(), &images);
    if answer.action == "chat" && guide.llm_available() {
        state.lock().unwrap().llm_requests += 1;
    }
    let (wav, duration, visemes) = synthesize(&piper, &tts_cache, &answer.reply);
    let name = format!("{}.wav", tts_key(&answer.reply));
    let (audio_name, audio) = audio_payload(&tts_cache, &name, &wav);
    tracing::info!(
        action = %answer.action,
        elapsed_ms = started.elapsed().as_millis() as u64,
        reply_chars = answer.reply.chars().count(),
        audio_bytes = wav.len(),
        "问答完成"
    );

    let out = json!({
        "reply": answer.reply,
        "action": answer.action,
        "emotion": answer.emotion,
        "gesture": answer.gesture,
        "exhibit": answer.exhibit,
        "exhibit_sequence": answer.exhibit_sequence,
        "audio": audio,
        "audio_name": audio_name,
        "duration": duration,
        "visemes": visemes,
        "elapsed_ms": started.elapsed().as_millis() as u64,
    });
    json_response(r, 200, &out.to_string())
}

/// 前端未捕获异常 / 未处理 Promise 拒绝回传入口。
/// 只接受单条小消息，字段逐项截断，避免日志被超长堆栈撑爆。
fn handle_frontend_log(r: &mut BufReader<TcpStream>, body: &[u8]) -> u16 {
    if body.len() > FRONTEND_LOG_MAX {
        return json_response(r, 413, &json!({"error": "日志内容过大"}).to_string());
    }
    let v: Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(_) => return json_response(r, 400, &json!({"error": "日志格式错误"}).to_string()),
    };
    let level = v["level"].as_str().unwrap_or("error");
    let message = truncate_chars(v["message"].as_str().unwrap_or(""), 2000);
    let source = truncate_chars(v["source"].as_str().unwrap_or(""), 160);
    let stack = truncate_chars(v["stack"].as_str().unwrap_or(""), 4000);
    let url = truncate_chars(v["url"].as_str().unwrap_or(""), 300);
    let line = v["line"].as_u64().unwrap_or(0);
    let col = v["col"].as_u64().unwrap_or(0);
    match level {
        "trace" | "debug" => {
            tracing::debug!(source = %source, url = %url, line, col, stack = %stack, "前端日志：{message}")
        }
        "info" => {
            tracing::info!(source = %source, url = %url, line, col, stack = %stack, "前端日志：{message}")
        }
        "warn" | "warning" => {
            tracing::warn!(source = %source, url = %url, line, col, stack = %stack, "前端日志：{message}")
        }
        _ => {
            tracing::error!(source = %source, url = %url, line, col, stack = %stack, "前端日志：{message}")
        }
    }
    json_response(r, 200, "{\"ok\":true}")
}

/// 按字符截断，保证不会切在多字节 UTF-8 中间。
fn truncate_chars(s: &str, max: usize) -> &str {
    if s.chars().count() <= max {
        return s;
    }
    let end = s.char_indices().nth(max).map(|(i, _)| i).unwrap_or(s.len());
    &s[..end]
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

pub(crate) fn tts_key(text: &str) -> String {
    // v9-huayan：讲解员语音换成 Piper zh_CN-huayan-medium 离线中文女声，
    // 不再使用 Edge/WinRT 小伊。缓存键必须变更，否则旧缓存（v8 小伊 mp3）
    // 会被误命中，现场听到的音色不一致。
    format!("{:016x}", fnv1a(&format!("v9-huayan{text}")))
}

/// 决定音频走内联 base64 还是 `/api/audio/<name>` 文件。
/// 静音回退音频刻意不落盘，此时必须内联，
/// 否则 `audio_name` 会指向一个不存在的文件，前端请求得到 404。
fn audio_payload(tts_cache: &Path, name: &str, wav: &[u8]) -> (String, String) {
    let on_disk = tts_cache.join(name).is_file();
    if on_disk && wav.len() > INLINE_AUDIO_LIMIT {
        (name.to_string(), String::new())
    } else {
        (String::new(), b64(wav))
    }
}

/// 最近一次神经语音合成的实时率（合成耗时 / 音频时长，越小越快），
/// 以百分之一为单位存整型，供 /api/stats 展示「TTS 实时率」。
static TTS_RTF_X100: AtomicU64 = AtomicU64::new(0);

/// 磁盘上已有的离线语音条数，供 /api/stats 核对赛前烘焙覆盖率。
fn cache_audio_count(dir: &Path) -> usize {
    fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| is_audio_name(&e.file_name().to_string_lossy()))
        .count()
}

/// 启动时记录已存在的音频文件名：主要是随 exe 释放的预生成语音，
/// 它们是断网仍可播放的本地 Piper 语音，LRU 清理绝不能碰。
fn snapshot_audio_names(dir: &Path) -> Vec<String> {
    fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| is_audio_name(n))
        .collect()
}

/// LRU 清理运行时动态合成的语音缓存：超出上限时按 mtime 从旧到新删，
/// 但绝不删除 protected（随 exe 释放的预生成语音 / 本次启动前已有的文件）。
/// 无上限会无限吃盘，现场演示越久越慢。
const TTS_CACHE_MAX_FILES: usize = 600;

fn prune_tts_cache(dir: &Path, protected: &[String]) {
    let Ok(rd) = fs::read_dir(dir) else { return };
    let mut entries: Vec<(std::time::SystemTime, PathBuf)> = rd
        .flatten()
        .filter(|e| is_audio_name(&e.file_name().to_string_lossy()))
        .filter(|e| !protected.contains(&e.file_name().to_string_lossy().into_owned()))
        .filter_map(|e| {
            let mtime = e.metadata().ok()?.modified().ok()?;
            Some((mtime, e.path()))
        })
        .collect();
    if entries.len() <= TTS_CACHE_MAX_FILES {
        return;
    }
    entries.sort_by_key(|(t, _)| *t);
    let overflow = entries.len() - TTS_CACHE_MAX_FILES;
    for (_, path) in entries.into_iter().take(overflow) {
        let _ = fs::remove_file(&path);
    }
    tracing::debug!(removed = overflow, "运行时语音缓存已按 LRU 清理");
}

/// 优先磁盘缓存；未命中按本地 ONNX（zh_CN-huayan-medium，Piper VITS）合成。
/// 彻底不可用时写入同长度静音 wav，保证前端 decodeAudioData 不失败、
/// 不弹「语音合成失败」，字幕/口型/镜头照常工作。
///
/// 所有语音合成均使用本地 ONNX 模型，不再调用云端 Edge TTS，
/// 从而实现“在线和离线统一为 zh_CN-huayan-medium 同一种声音”。
fn synthesize(piper: &PiperTts, tts_cache: &Path, text: &str) -> (Vec<u8>, f64, Vec<Value>) {
    let hex = tts_key(text);
    let name = format!("{hex}.wav");
    let path = tts_cache.join(&name);
    let mut inline: Option<Vec<u8>> = None;
    if path.exists() {
        tracing::debug!(chars = text.chars().count(), "TTS 命中磁盘缓存");
    } else {
        // 本地 VITS 神经语音（Piper huayan，ONNX Runtime 动态加载驱动）。
        let t0 = Instant::now();
        if let Some(wav) = piper.synth(text) {
            let dur = wav_duration(&wav);
            if dur > 0.2 {
                let rtf = (t0.elapsed().as_secs_f64() / dur * 100.0) as u64;
                TTS_RTF_X100.store(rtf.max(1), Ordering::Relaxed);
                tracing::debug!(
                    chars = text.chars().count(),
                    rtf = %format!("{:.2}", rtf as f64 / 100.0),
                    "VITS 离线合成完成（zh_CN-huayan-medium）"
                );
            }
            // 写盘只为下次命中缓存，结果不重要；本次一律把内存里的真实音频直接返回。
            // 旧实现写盘后不保留 wav，靠 fs::read 从磁盘读回；在部分环境下 fs::write
            // 静默失败（let _ = 吞掉错误），随后 fs::read 失败便落到 silent_wav(1.5)，
            // 导致所有未命中缓存的问题（识图文本必然如此）都只有文字、没有声音。
            let _ = fs::write(&path, &wav);
            inline = Some(wav);
        } else {
            // 本地 ONNX 不可用：宁可只显示文字，不换发音人。
            tracing::warn!(
                chars = text.chars().count(),
                "本地 ONNX TTS 不可用，返回静音音频（仅显示文字）"
            );
            let dur = (text.chars().count() as f64 * 0.18).clamp(1.2, 18.0);
            inline = Some(silent_wav(dur));
        }
    }
    let wav = inline.unwrap_or_else(|| fs::read(&path).unwrap_or_else(|_| silent_wav(1.5)));
    let duration = wav_duration(&wav);
    let dur = if duration > 0.1 { duration } else { 1.5 };
    (wav, (dur * 100.0).round() / 100.0, visemes_for(text, dur))
}

fn visemes_for(text: &str, dur: f64) -> Vec<Value> {
    let n = text
        .chars()
        .filter(|c| !c.is_whitespace())
        .count()
        .clamp(1, 120);
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
    if !(w.len() >= 44 && &w[0..4] == b"RIFF") {
        // Edge TTS 只输出 audio-24khz-48kbitrate-mono-mp3（CBR 48 kbps）。
        // 按固定码率估算时长即可驱动字幕/口型，前端 decodeAudioData 会自行解码。
        return if w.len() > 1024
            && (w.starts_with(b"ID3") || (w[0] == 0xff && (w[1] & 0xe0) == 0xe0))
        {
            (w.len() as f64 * 8.0 / 48_000.0).max(0.3)
        } else {
            0.0
        };
    }
    // canonical WAV 的 fmt 块：offset 28 是 byteRate（每秒字节数）
    let byte_rate = u32::from_le_bytes(w[28..32].try_into().unwrap_or([0; 4]));
    let mut off = 12;
    while off + 8 <= w.len() {
        let n = u32::from_le_bytes(w[off + 4..off + 8].try_into().unwrap_or([0; 4])) as usize;
        if &w[off..off + 4] == b"data" {
            return if byte_rate > 0 {
                n as f64 / byte_rate as f64
            } else {
                0.0
            };
        }
        off += 8 + n + (n % 2);
    }
    0.0
}

// ───────────────────────── HTTP 基础 ─────────────────────────

fn json_response(r: &mut BufReader<TcpStream>, code: u16, body: &str) -> u16 {
    write_response(
        r.get_mut(),
        code,
        reason(code),
        "application/json; charset=utf-8",
        body.as_bytes(),
        "",
    )
}

fn write_response(
    w: &mut TcpStream,
    code: u16,
    reason: &str,
    ctype: &str,
    body: &[u8],
    extra: &str,
) -> u16 {
    let head = format!(
        "HTTP/1.1 {code} {reason}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nAccess-Control-Allow-Origin: *\r\nConnection: close\r\n{extra}\r\n",
        body.len()
    );
    let _ = w.write_all(head.as_bytes());
    let _ = w.write_all(body);
    let _ = w.flush();
    code
}

fn reason(code: u16) -> &'static str {
    match code {
        200 => "OK",
        400 => "Bad Request",
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
    match p
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase()
        .as_str()
    {
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
    let Some((stem, ext)) = s.split_once('.') else {
        return false;
    };
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
        let n = ((c[0] as u32) << 16)
            | ((c.get(1).copied().unwrap_or(0) as u32) << 8)
            | c.get(2).copied().unwrap_or(0) as u32;
        s.push(T[((n >> 18) & 63) as usize] as char);
        s.push(T[((n >> 12) & 63) as usize] as char);
        s.push(if c.len() > 1 {
            T[((n >> 6) & 63) as usize] as char
        } else {
            '='
        });
        s.push(if c.len() > 2 {
            T[(n & 63) as usize] as char
        } else {
            '='
        });
    }
    s
}

// ───────────────────────── WebSocket（/ws/asr） ─────────────────────────

const WS_GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";

/// WebSocket 读线程只上报两种控制消息；识别结果由采集线程直接送回主循环。
enum WsControl {
    End,
    Closed,
}

fn handle_ws(r: &mut BufReader<TcpStream>, key: &str, asr: &Arc<Asr>) {
    let accept = b64(&sha1(format!("{key}{WS_GUID}").as_bytes()));
    let head = format!(
        "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {accept}\r\n\r\n"
    );
    if r.get_mut().write_all(head.as_bytes()).is_err() {
        return;
    }
    let _ = r.get_mut().flush();

    // 先启动采集：模型/麦克风不可用时直接回错误，不进入事件循环。
    let session = match asr.start_capture() {
        Ok(session) => session,
        Err(e) => {
            ws_text(
                r.get_mut(),
                &json!({"type": "error", "text": e}).to_string(),
            );
            // 前端收到 error 后仍会发 END；回一个空 final，让它立刻收尾。
            while let Some((op, payload)) = ws_frame(r) {
                if op == 8 {
                    break;
                }
                if op == 1 && payload == b"END" {
                    ws_text(
                        r.get_mut(),
                        &json!({"type": "final", "text": ""}).to_string(),
                    );
                    break;
                }
            }
            let _ = r.get_mut().shutdown(Shutdown::Both);
            return;
        }
    };

    // 客户端在这条连接上只会发 END / close；用独立读线程，主线程才能同时
    // 转发 cpal 采集线程的 partial/final（同一个 TcpStream 只由一个线程写）。
    let buffered = r.buffer().to_vec();
    let Ok(read_half) = r.get_ref().try_clone() else {
        session.cancel();
        return;
    };
    let (ctrl_tx, ctrl_rx) = mpsc::channel::<WsControl>();
    let reader = std::thread::Builder::new()
        .name("qx-asr-ws".into())
        .spawn(move || {
            let mut rr = BufReader::new(std::io::Cursor::new(buffered).chain(read_half));
            loop {
                match ws_frame(&mut rr) {
                    Some((8, _)) => {
                        let _ = ctrl_tx.send(WsControl::Closed);
                        break;
                    }
                    Some((1, payload)) if payload == b"END" => {
                        let _ = ctrl_tx.send(WsControl::End);
                        break;
                    }
                    Some(_) => {}
                    None => {
                        let _ = ctrl_tx.send(WsControl::Closed);
                        break;
                    }
                }
            }
        });
    if reader.is_err() {
        session.cancel();
        let _ = r.get_mut().shutdown(Shutdown::Both);
        return;
    }

    let mut finishing = false;
    let mut finish_deadline: Option<Instant> = None;
    loop {
        // 先排空识别事件，避免 final 已到达却先看到「线程结束」而补空文本。
        let mut saw_final = false;
        while let Some(event) = session.try_event() {
            match event {
                AsrEvent::Partial(text) => {
                    ws_text(
                        r.get_mut(),
                        &json!({"type": "partial", "text": text}).to_string(),
                    );
                }
                AsrEvent::Error(text) => {
                    ws_text(
                        r.get_mut(),
                        &json!({"type": "error", "text": text}).to_string(),
                    );
                }
                AsrEvent::Final { text, audio_ms } => {
                    tracing::info!(
                        audio_ms = audio_ms,
                        text_chars = text.chars().count(),
                        "语音识别完成"
                    );
                    ws_text(
                        r.get_mut(),
                        &json!({"type": "final", "text": text}).to_string(),
                    );
                    saw_final = true;
                    break;
                }
            }
        }
        if saw_final {
            break;
        }
        if session.is_finished() {
            // 采集线程异常退出且没有 final：补空 final，避免前端等满超时。
            ws_text(
                r.get_mut(),
                &json!({"type": "final", "text": ""}).to_string(),
            );
            break;
        }
        if let Some(deadline) = finish_deadline {
            if Instant::now() >= deadline {
                session.cancel();
                ws_text(
                    r.get_mut(),
                    &json!({"type": "error", "text": "语音识别超时，请重试"}).to_string(),
                );
                ws_text(
                    r.get_mut(),
                    &json!({"type": "final", "text": ""}).to_string(),
                );
                break;
            }
        }

        // 录音阶段每 50 ms 看一下控制帧；收尾阶段只等 final，降低空转。
        let wait = if finishing {
            Duration::from_millis(100)
        } else {
            Duration::from_millis(50)
        };
        match ctrl_rx.recv_timeout(wait) {
            Ok(WsControl::End) if !finishing => {
                finishing = true;
                // 首次推理要加载 1.8 GB 权重；给最终转写留足 30 秒。
                finish_deadline = Some(Instant::now() + Duration::from_secs(30));
                session.finish();
            }
            Ok(WsControl::End) => {}
            Ok(WsControl::Closed) => {
                session.cancel();
                break;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                if !finishing {
                    session.cancel();
                    break;
                }
                // 读线程已退出但采集线程还在收尾：慢速轮询 final，避免空转。
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    }
    let _ = r.get_mut().shutdown(Shutdown::Both);
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
    let mut h: [u32; 5] = [
        0x6745_2301,
        0xEFCD_AB89,
        0x98BA_DCFE,
        0x1032_5476,
        0xC3D2_E1F0,
    ];
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

    #[test]
    fn truncate_chars_respects_utf8_boundaries() {
        assert_eq!(truncate_chars("侨乡江门", 2), "侨乡");
        assert_eq!(truncate_chars("abc", 10), "abc");
        assert_eq!(truncate_chars("", 3), "");
    }

    /// 防止回归：LlmConfig 若用 derive(Default)，地址与模型名会变成空串，
    /// 双击启动时 Ollama 客户端连到空地址、请求体 model 为空，所有自由问答
    /// 静默退回兜底话术（曾导致「这个我一时答不上来」+ 识图失效）。
    #[test]
    fn default_llm_config_points_at_real_ollama() {
        let cfg = LlmConfig::default();
        let d = OllamaClient::default();
        assert_eq!(cfg.ollama_url, d.host);
        assert_eq!(cfg.ollama_model, "qwen3.5:4b");
        assert!(
            cfg.ollama_url.contains("11434"),
            "默认端口必须是 Ollama 的标准端口 11434"
        );
        assert!(cfg.local_model.is_none());

        // 即便手工构造了空配置，展示名与客户端也必须被兜底，不能留空
        let empty = LlmConfig {
            backend: LlmBackend::Auto,
            ollama_url: String::new(),
            ollama_model: String::new(),
            local_model: None,
            local_mmproj: None,
            embedded_model: false,
        };
        assert_eq!(describe_llm(&empty).1, "qwen3.5:4b");
        let _client = build_llm(&empty); // 不应 panic，也不应连到空地址
    }

    /// 注：Edge TTS 回退已移除，`keep_xiaoyi_only` 不再存在，严格离线守卫测试随功能删除。

    /// /api/stats 的覆盖率数字必须只数合法的缓存音频文件，不能把 .txt / 报告
    /// 之类的杂项算进去，否则会虚报「离线语音已备齐」。
    #[test]
    fn cache_audio_count_only_counts_valid_cache_names() {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir =
            std::env::temp_dir().join(format!("qx-cache-count-{}-{nanos}", std::process::id()));
        fs::create_dir_all(&dir).expect("创建测试临时目录");
        for n in [
            "0e5bbf57873c8703.wav",
            "ABCDEF0123456789.wav",
            "notes.txt",
            "short.wav",
        ] {
            fs::write(dir.join(n), b"RIFF").unwrap();
        }
        assert_eq!(cache_audio_count(&dir), 2);
        assert_eq!(cache_audio_count(&dir.join("definitely-missing")), 0);
        fs::remove_dir_all(&dir).expect("清理测试临时目录");
    }

    /// 严格离线模式的静音刻意不落盘；若仍按大小返回 audio_name，
    /// 前端请求 /api/audio/ 就是 404。这是端到端实测抓到的缺陷。
    #[test]
    fn audio_payload_never_points_at_a_missing_file() {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir =
            std::env::temp_dir().join(format!("qx-audio-payload-{}-{nanos}", std::process::id()));
        fs::create_dir_all(&dir).expect("创建测试临时目录");
        let name = "0123456789abcdef.wav";
        let big = vec![0u8; INLINE_AUDIO_LIMIT + 1];
        let (n, a) = audio_payload(&dir, name, &big);
        assert!(n.is_empty(), "缓存文件不存在时必须内联音频");
        assert!(!a.is_empty());
        fs::write(dir.join(name), b"RIFF").unwrap();
        let (n2, a2) = audio_payload(&dir, name, &big);
        assert_eq!(n2, name, "已落盘的大音频走文件路由");
        assert!(a2.is_empty());
        let (n3, a3) = audio_payload(&dir, name, b"RIFF");
        assert!(n3.is_empty(), "小音频保持内联");
        assert!(!a3.is_empty());
        fs::remove_dir_all(&dir).expect("清理测试临时目录");
    }

    /// 端到端冒烟：Piper 神经语音预热 → 问答 → 合成 → 校验 wav 与缓存。
    /// 顺带把 v8 键的 KB 语音预热进 tts-cache，正式运行首次提问即命中缓存。
    #[test]
    fn tts_worker_ask_and_synth_smoke() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../");
        let state = Arc::new(Mutex::new(
            AppState::new(root, "qiaoxiang", LlmConfig::default()).expect("AppState"),
        ));
        let (piper, tts_cache, guide) = {
            let st = state.lock().unwrap();
            (
                Arc::clone(&st.piper),
                st.tts_cache.clone(),
                Arc::clone(&st.guide),
            )
        };
        piper.preload();
        assert!(piper.load_ms() > 0, "Piper 神经语音应在预热后就绪");

        let answer = guide.answer("介绍开平碉楼", None, false, None);
        assert!(!answer.reply.is_empty(), "知识库应命中碉楼条目");
        println!(
            "reply({}字): {}",
            answer.reply.chars().count(),
            answer.reply
        );

        let hex = tts_key(&answer.reply);
        let (wav, dur, visemes) = synthesize(&piper, &tts_cache, &answer.reply);
        assert!(wav.len() > 44, "合成产物应是有效 wav");
        assert!(dur > 0.5, "时长应来自真实 wav 帧数，实测 {dur}s");
        assert!(!visemes.is_empty(), "口型轨道应与文本等长");
        let path = tts_cache.join(format!("{hex}.wav"));
        assert!(path.is_file(), "v8 缓存文件应已落盘");

        // 神经语音口径：约 5 字/秒，不应出现系统语音那种拖沓
        let rate = answer.reply.chars().count() as f64 / dur;
        assert!(
            rate > 3.5,
            "语速应接近真人讲解（>3.5 字/秒），实测 {rate:.1}"
        );

        // 二次合成同文本：直接命中磁盘缓存
        let (wav2, dur2, _) = synthesize(&piper, &tts_cache, &answer.reply);
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
        let Ok(rd) = std::fs::read_dir(&dir) else {
            return;
        };
        if !rd
            .flatten()
            .any(|e| e.path().extension().is_some_and(|x| x == "wav"))
        {
            return;
        }
        let manifest = root.join("voice_manifest.json");
        let Ok(raw) = std::fs::read_to_string(&manifest) else {
            return;
        };
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
        println!(
            "预生成语音 {} 条缓存键全部匹配",
            list.as_array().unwrap().len()
        );
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

        let guide =
            Guide::new(Theme::load(theme_path.to_string_lossy().to_string()).expect("加载主题包"));
        let prebuilt = root.join("prebuilt-voices");
        for (i, q) in quiz.iter().enumerate() {
            let opts = q["options"].as_array().expect("题目缺少 options");
            let answer = q["answer"].as_u64().expect("题目缺少 answer") as usize;
            assert!(
                opts.len() >= 2 && answer < opts.len(),
                "第 {i} 题选项或答案越界"
            );
            let followup = q["followup"].as_str().expect("题目缺少 followup");
            let a = guide.answer(followup, None, false, None);
            assert!(!a.reply.is_empty(), "第 {i} 题追问「{followup}」没有答案");
            assert_ne!(
                a.action, "chat",
                "第 {i} 题追问「{followup}」未命中知识库，会走大模型"
            );
            let key = format!("{}.wav", tts_key(&a.reply));
            assert!(
                prebuilt.join(&key).is_file(),
                "第 {i} 题追问「{followup}」缺少预生成语音 {key}，请重跑 tools/prebuild_voices.py"
            );
        }
        println!(
            "互动题库 {} 题：追问全部命中知识库且已有标准普通话语音",
            quiz.len()
        );
    }
}
