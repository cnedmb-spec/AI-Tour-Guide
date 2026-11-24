#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

//! 侨乡 · 江门 AI 智能讲解员 —— 原生桌面端（gpui-kit 窗口 + WebView 渲染原网页）
//!
//! 做法：gpui-kit 提供原生窗口，窗口内嵌 WebView 视图（Windows 用 WebView2，
//! Linux 用 WebKitGTK），渲染【原封不动】的
//! `web/` 页面（Three.js + VRM 数字人 + 聊天 UI）；前端一个字节都没改，因此
//! 画面、数字人、镜头、字幕、展台贴图与原网页一模一样。后端接口全部由 Rust
//! 实现（主题包 / 知识库路由 / Ollama / 语音 / 限流 / WebSocket），不依赖 Python。
//!
//! 自包含：`web/` 与 `themes/` 在编译期由 build.rs 压进 exe，首次运行释放到
//! 系统临时目录，因此单个 exe 拷到任何位置都能双击运行，无需外部文件夹。

mod asr;
#[cfg(feature = "local-llm")]
mod embedded_model;
mod http;
mod logging;
mod online;
mod piper;
mod vision_route;
mod voice_bake;

/// 本地 GGUF 大模型（内嵌推理，无需 Ollama / 云端）。
/// 仅在 `cargo build --features local-llm` 时参与编译，且构建机需装有 C/C++ 工具链。
#[cfg(feature = "local-llm")]
mod local_llm;

use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use gpui_kit::*;
use gpui_webview::WebView;

const TITLE: &str = "侨乡 · 江门 AI 智能讲解员";

/// 运行时资产释放目录名。`voice_bake` 也用它定位演示时在线合成的语音缓存；
/// 升版本时两处必须同步，否则烘焙会去旧目录找缓存。
pub(crate) const BUNDLE_DIR_NAME: &str = "qx-guide-bundle-v11";

/// 编译期打进 exe 的页面 + 主题包（build.rs 生成）。
const BUNDLE: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/bundle.zip"));

/// 把内嵌资产释放到可写目录，返回作为 HTTP 服务根的路径。
/// 只在文件缺失或大小不一致时写入，重复启动不重复解压。
/// 优先用系统临时目录（支持「exe 拷到任何位置都能双击运行」）；
/// 若临时目录不可写（权限 / 沙箱限制），回退到 exe 自身所在目录，保证一定能释放。
fn extract_bundle() -> PathBuf {
    // v3：新增 47MB 吉祥物模型与前端适配层。换目录名强制全量释放，
    // 避免「已存在且大小一致就跳过」让旧页面残留、新 mascot.glb 又不解压。
    // v5：主镜头取景占比改为 0.60（讲解员缩小、露出后排展品），强制刷新临时解压目录
    // v6：46 条预生成语音换成卡通活泼女声、缓存键升 v8，强制释放新 tts-cache 音频
    // v7：吉祥物模式首帧即隐藏 VRM 人形网格 + 加载层等吉祥物就位再揭开，消除开场旧人物闪现
    // v8：左上角加品牌标志（assets/brand/*.svg），强制释放新页面避免旧 index.html 残留
    // v9：新增 asr/（whisper.dll + ggml*.dll + base 量化模型），换目录避免旧版残留 DLL 干扰
    // v11：语音识别改为 cpal 采集 + valle-asr/Qwen3-ASR，旧 whisper 资产不再随包。
    //      模型默认不嵌入 exe（>2 GB 的 exe 在部分 Windows 上无法启动），
    //      从 exe 同级 asr/ 或 QX_ASR_MODEL_DIR 加载；新版本使用独立目录避免混用旧资源。

    // 候选释放目录：先系统临时目录，再 exe 同级目录。
    let temp_dir = std::env::temp_dir();
    let mut candidates: Vec<PathBuf> = vec![temp_dir.clone()];
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            let p = parent.to_path_buf();
            if !candidates.contains(&p) {
                candidates.push(p);
            }
        }
    }

    // 不自动删除其他版本的 bundle：另一个仍在运行的实例可能正在使用它。
    // 版本化目录已避免新旧资源混用；旧目录由用户或系统清理。

    for base in &candidates {
        let dir = base.join(BUNDLE_DIR_NAME);
        // 先尝试建目录；建不出来说明该位置不可写，跳到下一个候选。
        if let Err(e) = fs::create_dir_all(&dir) {
            tracing::warn!(error = %e, path = %dir.display(), "释放目录不可写，尝试下一个候选位置");
            continue;
        }
        // 再用探针文件确认目录真的可写：目录已存在但个别文件被锁/只读时，
        // 逐文件写盘会在中途失败；先探测一次，失败就整体跳到下一个候选。
        if !bundle_dir_writable(&dir) {
            tracing::warn!(path = %dir.display(), "释放目录不可写，尝试下一个候选位置");
            continue;
        }
        if extract_into(&dir) {
            tracing::info!(root = %dir.display(), "内嵌页面与主题包已释放");
            return dir;
        }
        tracing::warn!(path = %dir.display(), "该位置解压失败，尝试下一个候选位置");
    }

    // 所有候选都失败：返回临时目录兜底（后续 load_theme 会报错并暴露真实原因）。
    let fallback = temp_dir.join(BUNDLE_DIR_NAME);
    tracing::error!(path = %fallback.display(), "所有候选释放位置均失败，回退到临时目录");
    fallback
}

/// 用探针文件确认释放目录可写。目录已存在但个别文件被锁/只读时，
/// 逐文件写盘会在中途失败；先探测一次，失败就整体跳到下一个候选位置。
fn bundle_dir_writable(dir: &Path) -> bool {
    let probe = dir.join(format!(".qx-bundle-probe-{}", std::process::id()));
    match std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&probe)
    {
        Ok(_) => {
            let _ = fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

/// 把内嵌资产解压到指定目录，返回是否成功（解压过程中是否有写盘错误）。
fn extract_into(dir: &Path) -> bool {
    let reader = std::io::Cursor::new(BUNDLE);
    let mut archive = match zip::ZipArchive::new(reader) {
        Ok(a) => a,
        Err(e) => {
            tracing::error!(error = %e, "内嵌资产 zip 损坏，无法释放页面资产");
            return false;
        }
    };
    tracing::info!(entries = archive.len(), "内嵌 zip 已打开");
    let mut ok = true;
    for i in 0..archive.len() {
        let mut f = match archive.by_index(i) {
            Ok(f) => f,
            Err(e) => {
                tracing::error!(error = %e, index = i, "by_index 失败");
                ok = false;
                continue;
            }
        };
        let name = f.name().to_string();
        let out = dir.join(name.replace('\\', "/"));
        if f.is_dir() {
            if let Err(e) = fs::create_dir_all(&out) {
                tracing::error!(error = %e, path = %out.display(), "创建目录失败");
                ok = false;
            }
        } else {
            if let Some(parent) = out.parent() {
                if let Err(e) = fs::create_dir_all(parent) {
                    tracing::error!(error = %e, path = %parent.display(), "创建父目录失败");
                    ok = false;
                }
            }
            // 已存在且大小一致就跳过：几十 MB 资源重复写盘白白拖慢每次启动。
            let fresh = fs::metadata(&out)
                .map(|m| m.len() == f.size())
                .unwrap_or(false);
            if fresh {
                continue;
            }
            // 流式写出：Qwen3-ASR 权重有 1.8 GB，整文件读进 Vec 会让启动峰值内存翻倍。
            match fs::File::create(&out) {
                Ok(mut out_file) => {
                    if let Err(e) = std::io::copy(&mut f, &mut out_file) {
                        tracing::error!(error = %e, path = %out.display(), "写出文件失败");
                        ok = false;
                    }
                }
                Err(e) => {
                    tracing::error!(error = %e, path = %out.display(), "创建文件失败，跳过该释放位置");
                    return false;
                }
            }
        }
    }
    ok
}

pub struct AppView {
    webview: Entity<WebView>,
    url: std::sync::Arc<String>,
}

impl Render for AppView {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        // 全屏 WebView：与网页版外观一致（窗口自带标题栏用于关闭/缩放）。
        div()
            .size_full()
            .bg(rgb(0x0c0a08))
            .child(self.webview.clone())
    }
}

fn main() {
    // WebView2 子视图在 Windows 上必须先关掉 GPUI 的 DirectComposition，否则不绘制。
    #[cfg(target_os = "windows")]
    unsafe {
        std::env::set_var("GPUI_DISABLE_DIRECT_COMPOSITION", "true");
    }

    // Linux 上 WebKitGTK 的 DMABUF 渲染器在 NVIDIA（尤其 XWayland / PRIME 混合显卡）
    // 下分配 GBM buffer 会失败（"Failed to create GBM buffer ... Invalid argument"），
    // 表现为窗口黑屏但页面请求照常发出。禁用 DMABUF 渲染器回退到 GL 路径即可正常显示。
    #[cfg(target_os = "linux")]
    unsafe {
        std::env::set_var("WEBKIT_DISABLE_DMABUF_RENDERER", "1");
    }

    // 日志必须最先初始化：后面的资源释放、模型加载、HTTP 服务都靠它留痕。
    // PowerShell/System.Speech 时代是 Windows 专属路径；现在改为平台用户数据目录。
    // GUI 子系统没有控制台，日志默认落盘到用户数据目录 QiaoxiangGuide\logs。
    let _log_guard = logging::init();
    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        log_dir = %logging::dir().map(|p| p.display().to_string()).unwrap_or_default(),
        "侨乡 AI 智能讲解员启动"
    );

    // 释放内嵌页面资产（首次或版本升级时），作为 HTTP 服务根。
    let root = extract_bundle();
    tracing::info!(root = %root.display(), "内嵌页面与主题包已释放");
    let preferred = std::env::var("QX_PORT")
        .ok()
        .and_then(|s| s.parse::<u16>().ok())
        .unwrap_or(8765);

    // ── 解析 LLM 启动参数 ─────────────────────────────────────
    //   --model <GGUF路径>   使用本地 GGUF 模型（需 --features local-llm 构建）
    //   --ollama-url <URL>   Ollama / llama-server 兼容协议地址（默认 http://127.0.0.1:11434）
    //   --ollama-model <名>  模型名（默认 qwen3.5:4b）
    //   --llm-backend <auto|ollama|llama|local|none>
    // 默认行为与旧版一致：走本机 Ollama，模型 qwen3.5:4b。
    let mut cfg = http::LlmConfig::default();
    let mut bake_voices = false;
    let args: Vec<String> = std::env::args().collect();
    let mut i = 1usize;
    while i < args.len() {
        match args[i].as_str() {
            "--bake-voices" => {
                // 赛前跑一次：用本地 Piper 把演示语音固化到预生成目录。
                bake_voices = true;
                i += 1;
            }
            "--model" => {
                if let Some(v) = args.get(i + 1) {
                    cfg.backend = http::LlmBackend::Local;
                    cfg.local_model = Some(PathBuf::from(v));
                    i += 2;
                } else {
                    i += 1;
                }
            }
            "--ollama-url" => {
                if let Some(v) = args.get(i + 1) {
                    cfg.ollama_url = v.clone();
                    if cfg.backend == http::LlmBackend::Auto {
                        cfg.backend = http::LlmBackend::Ollama;
                    }
                    i += 2;
                } else {
                    i += 1;
                }
            }
            "--ollama-model" => {
                if let Some(v) = args.get(i + 1) {
                    cfg.ollama_model = v.clone();
                    i += 2;
                } else {
                    i += 1;
                }
            }
            "--llm-backend" => {
                if let Some(v) = args.get(i + 1) {
                    cfg.backend = match v.as_str() {
                        "ollama" => http::LlmBackend::Ollama,
                        "llama" => http::LlmBackend::LlamaServer,
                        "local" => http::LlmBackend::Local,
                        "none" => http::LlmBackend::None,
                        _ => cfg.backend,
                    };
                    i += 2;
                } else {
                    i += 1;
                }
            }
            _ => i += 1,
        }
    }

    // ── 内嵌模型（Local 版默认，零 Ollama 依赖）──────────────────
    // exe 带 PE overlay 时自动改用自带的 Qwen2.5-VL-3B：现场只需一个文件，
    // 不需要装 Ollama、不需要 --model、不需要把 3 GiB 模型另拷一份。
    // QX_NO_EMBEDDED_MODEL=1 可强制关掉，回退到原来的 Ollama / --model 行为。
    #[cfg(feature = "local-llm")]
    {
        // 只有非空且非 0 才算「关闭」；空值（例如脚本里误写 QX_NO_EMBEDDED_MODEL=）
        // 不应把内嵌模型关掉。
        let no_embed =
            std::env::var("QX_NO_EMBEDDED_MODEL").is_ok_and(|v| !v.is_empty() && v != "0");
        if cfg.local_model.is_none() && !no_embed {
            if let Some(m) = crate::embedded_model::release() {
                cfg.backend = http::LlmBackend::Local;
                cfg.local_mmproj = Some(m.mmproj);
                cfg.local_model = Some(m.model);
                cfg.embedded_model = true;
                tracing::info!("Local 版已启用内置 Qwen2.5-VL-3B（无需 Ollama / --model）");
            }
        }
    }

    // ── 本地模型自检（仅 `local-llm` 构建可用）────────────────────
    // 不创建窗口、不启 HTTP，直接加载 GGUF 生成一段话，用于无桌面环境验证
    // 内嵌推理链路是否真的可用：QX_LLM_SELFTEST=1 qx-desktop.exe --model <GGUF>
    // 默认构建（无此 feature）下整段代码不参与编译，行为完全不变。
    #[cfg(feature = "local-llm")]
    if std::env::var_os("QX_LLM_SELFTEST").is_some() {
        use qx_core::llm::{LlmClient, Message};
        match &cfg.local_model {
            Some(p) => {
                tracing::info!(model = %p.display(), "本地模型自检：开始加载");
                match crate::local_llm::LocalLlm::load(
                    p,
                    cfg.local_mmproj.as_deref(),
                    cfg.kb_prompt.clone(),
                ) {
                    Ok(llm) => {
                        tracing::info!("本地模型自检：模型就绪，开始真实生成");
                        let msgs = vec![
                            Message::system(
                                "你是 AI 智能讲解员小侨，在侨乡江门展厅为观众讲解。\
                                 只谈展馆相关内容，回答不超过 60 字，口语化、不分点。",
                            ),
                            Message::user("请用一句话介绍你自己，并说说你负责讲解什么。"),
                        ];
                        match llm.chat(&msgs, 0.7, 64) {
                            Ok(ans) => tracing::info!(answer = %ans, "本地模型自检：生成成功"),
                            Err(e) => tracing::error!(error = %e, "本地模型自检：生成失败"),
                        }
                    }
                    Err(e) => tracing::error!(error = %e, "本地模型自检：模型加载失败"),
                }
            }
            None => tracing::error!("本地模型自检：需要 --model <GGUF 路径>"),
        }
        return;
    }

    // 离线 Piper 语音烘焙：不建窗口、不起 HTTP，只合成语音并写报告文件
    // （GUI 子系统进程没有控制台，stdout 不可见，报告落盘才可核对）。
    // 默认写回编译时的源码根 qx-rust/；换机器可用 QX_VOICE_BAKE_DIR 指过去。
    if bake_voices {
        let src = std::env::var("QX_VOICE_BAKE_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../"));
        let report = voice_bake::run(&src, &cfg);
        let report_path = src.join("voice_bake_report.txt");
        let _ = fs::write(&report_path, &report);
        tracing::info!(path = %report_path.display(), "语音烘焙完成");
        eprintln!("{report}");
        return;
    }

    // 端口被旧实例占用时顺延，保证同时开着旧桌面端也不会冲突。
    let port = (preferred..preferred + 8)
        .find_map(|p| http::spawn(root.clone(), p, cfg.clone()).ok())
        .expect("无法启动 Rust 本地服务（8765-8772 均被占用）");

    // 无桌面会话的后端自检模式：只起 HTTP、不创建 WebView2 窗口，便于
    // 直接 curl /api/ask 验证接口（含识图）。不设 QX_HEADLESS 时行为完全不变。
    if std::env::var_os("QX_HEADLESS").is_some() {
        tracing::info!(url = %format!("http://127.0.0.1:{port}/"), "无头模式服务已就绪");
        // GUI 子系统没有可用的 stderr，直接 eprintln! 会 panic；写失败时静默忽略。
        let _ = writeln!(
            std::io::stderr(),
            "[headless] 服务已就绪：http://127.0.0.1:{port}/ （Ctrl+C 结束）"
        );
        loop {
            std::thread::sleep(std::time::Duration::from_secs(3600));
        }
    }

    let url = std::sync::Arc::new(format!("http://127.0.0.1:{port}/"));
    tracing::info!(url = %url.as_str(), "桌面端窗口即将打开");
    let for_window = url.clone();

    // WebView 在 Linux 上必须挂 X11 窗口：gpui 默认按会话类型选择 Wayland/X11，
    // Wayland 会话下拿不到 Xlib 窗口句柄，wry 直接报错（本机为 Wayland 时复现）。
    // 显式固定 X11（XWayland 也能跑）；Windows / macOS 走默认 application()。
    #[cfg(target_os = "linux")]
    let app = gpui_kit::platform::linux(gpui_kit::WindowingModes::X11);
    #[cfg(not(target_os = "linux"))]
    let app = gpui_kit::application();
    app.with_assets(gpui_kit::assets::Assets).run(move |cx| {
        gpui_kit::init(cx);
        let bounds = Bounds::centered(None, size(px(1440.), px(900.)), cx);
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            #[cfg(windows)]
            titlebar: Some(TitlebarOptions {
                title: Some(TITLE.into()),
                ..Default::default()
            }),
            ..Default::default()
        };
        gpui_kit::open_window(options, cx, move |window, cx| {
            let webview = cx.new(|cx| {
                WebView::build(wry::WebViewBuilder::new(), window, cx)
                    .expect("无法创建 WebView2 视图")
            });
            webview.update(cx, |view, _| view.load_url(&url));
            cx.new(|_| AppView {
                webview,
                url: for_window,
            })
        })
        .expect("无法打开窗口");
    });
    tracing::info!("桌面端已正常退出");
}
