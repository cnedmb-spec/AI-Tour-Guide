#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

//! 侨乡 · 江门 AI 智能讲解员 —— 原生桌面端（gpui-kit 窗口 + WebView2 渲染原网页）
//!
//! 做法：gpui-kit 提供原生窗口，窗口内嵌 WebView2 视图，渲染【原封不动】的
//! `web/` 页面（Three.js + VRM 数字人 + 聊天 UI）；前端一个字节都没改，因此
//! 画面、数字人、镜头、字幕、展台贴图与原网页一模一样。后端接口全部由 Rust
//! 实现（主题包 / 知识库路由 / Ollama / 语音 / 限流 / WebSocket），不依赖 Python。
//!
//! 自包含：`web/` 与 `themes/` 在编译期由 build.rs 压进 exe，首次运行释放到
//! 系统临时目录，因此单个 exe 拷到任何位置都能双击运行，无需外部文件夹。

mod http;
mod piper;

use std::fs;
use std::io::Read;
use std::path::PathBuf;

use gpui_kit::*;
use gpui_webview::WebView;

const TITLE: &str = "侨乡 · 江门 AI 智能讲解员";

/// 编译期打进 exe 的页面 + 主题包（build.rs 生成）。
const BUNDLE: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/bundle.zip"));

/// 把内嵌资产释放到临时目录，返回作为 HTTP 服务根的路径。
/// 只在文件缺失或大小不一致时写入，重复启动不重复解压。
fn extract_bundle() -> PathBuf {
    // v3：新增 47MB 吉祥物模型与前端适配层。换目录名强制全量释放，
    // 避免「已存在且大小一致就跳过」让旧页面残留、新 mascot.glb 又不解压。
    // v5：主镜头取景占比改为 0.60（讲解员缩小、露出后排展品），强制刷新临时解压目录
    // v6：46 条预生成语音换成卡通活泼女声、缓存键升 v8，强制释放新 tts-cache 音频
    // v7：吉祥物模式首帧即隐藏 VRM 人形网格 + 加载层等吉祥物就位再揭开，消除开场旧人物闪现
    let dir = std::env::temp_dir().join("qx-guide-bundle-v7");
    let _ = fs::create_dir_all(&dir);
    let reader = std::io::Cursor::new(BUNDLE);
    let mut archive = zip::ZipArchive::new(reader).expect("内嵌资产损坏");
    for i in 0..archive.len() {
        let mut f = match archive.by_index(i) {
            Ok(f) => f,
            Err(_) => continue,
        };
        let name = f.name().to_string();
        let out = dir.join(name.replace('\\', "/"));
        if f.is_dir() {
            let _ = fs::create_dir_all(&out);
        } else {
            if let Some(parent) = out.parent() {
                let _ = fs::create_dir_all(parent);
            }
            // 已存在且大小一致就跳过：几十 MB 资源重复写盘白白拖慢每次启动。
            let fresh = fs::metadata(&out)
                .map(|m| m.len() == f.size())
                .unwrap_or(false);
            if fresh {
                continue;
            }
            let mut buf = Vec::with_capacity(f.size() as usize);
            let _ = f.read_to_end(&mut buf);
            let _ = fs::write(&out, &buf);
        }
    }
    dir
}

pub struct AppView {
    webview: Entity<WebView>,
    url: std::sync::Arc<String>,
}

impl Render for AppView {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        // 全屏 WebView：与网页版外观一致（窗口自带标题栏用于关闭/缩放）。
        div().size_full().bg(rgb(0x0c0a08)).child(self.webview.clone())
    }
}

fn main() {
    // WebView2 子视图在 Windows 上必须先关掉 GPUI 的 DirectComposition，否则不绘制。
    #[cfg(target_os = "windows")]
    unsafe {
        std::env::set_var("GPUI_DISABLE_DIRECT_COMPOSITION", "true");
    }

    // 释放内嵌页面资产（首次或版本升级时），作为 HTTP 服务根。
    let root = extract_bundle();
    let preferred = std::env::var("QX_PORT")
        .ok()
        .and_then(|s| s.parse::<u16>().ok())
        .unwrap_or(8765);

    // 端口被旧实例占用时顺延，保证同时开着旧桌面端也不会冲突。
    let port = (preferred..preferred + 8)
        .find_map(|p| http::spawn(root.clone(), p).ok())
        .expect("无法启动 Rust 本地服务（8765-8772 均被占用）");
    let url = std::sync::Arc::new(format!("http://127.0.0.1:{port}/"));
    let for_window = url.clone();

    gpui_kit::application()
        .with_assets(gpui_kit::assets::Assets)
        .run(move |cx| {
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
                cx.new(|_| AppView { webview, url: for_window })
            })
            .expect("无法打开窗口");
        });
}
