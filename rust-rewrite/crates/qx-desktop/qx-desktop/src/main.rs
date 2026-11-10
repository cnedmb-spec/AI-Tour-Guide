#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

//! 侨乡 · 江门 AI 智能讲解员 —— Rust 版桌面端（零 Python）
//!
//! 与旧版「看起来一模一样」的做法：**不重画 3D**。
//! 本进程内起一个 Rust HTTP 服务，原样托管 `app/` 里的 Three.js + VRM 网页，
//! 再用 gpui-kit / gpui-webview（Windows 走 WebView2）把同一个页面嵌进原生窗口。
//! 后端接口全部由 Rust 实现（主题包、知识库路由、Ollama、语音、限流、WebSocket），
//! 前端一个字节都没改，因此画面、数字人、镜头、字幕、展台贴图与原网页完全一致。

mod http;

use std::{path::PathBuf, sync::Arc};

use gpui_kit::*;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{h_flex, v_flex};
use gpui_webview::WebView;

const TITLE: &str = "侨乡 · 江门 AI 智能讲解员";

fn project_root() -> PathBuf {
    // 自包含版把网页放在 rust-rewrite/web、主题包放在 rust-rewrite/themes；
    // 仍兼容旧布局（项目根的 app/ + themes/），方便对照与回滚。
    let ok = |p: &PathBuf| {
        (p.join("web/index.html").is_file() || p.join("app/index.html").is_file())
            && p.join("themes/qiaoxiang/theme.json").is_file()
    };
    if let Ok(root) = std::env::var("QIAOXIANG_ROOT") {
        let p = PathBuf::from(root);
        if ok(&p) {
            return p;
        }
    }
    let mut candidates = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        let mut d = exe.parent().map(|p| p.to_path_buf());
        for _ in 0..7 {
            if let Some(x) = d.clone() {
                candidates.push(x.clone());
                d = x.parent().map(|p| p.to_path_buf());
            }
        }
    }
    if let Ok(cwd) = std::env::current_dir() {
        let mut d = Some(cwd);
        for _ in 0..5 {
            if let Some(x) = d.clone() {
                candidates.push(x.clone());
                d = x.parent().map(|p| p.to_path_buf());
            }
        }
    }
    for p in &candidates {
        if ok(p) {
            return p.clone();
        }
    }
    // 开发期兜底：crates/qx-desktop → crates → rust-rewrite → 项目根
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .to_path_buf()
}

pub struct AppView {
    webview: Entity<WebView>,
    url: Arc<String>,
}

impl AppView {
    fn reload(&mut self, _: &ClickEvent, _: &mut Window, cx: &mut Context<Self>) {
        let url = self.url.clone();
        self.webview.update(cx, |view, _| view.load_url(&url));
    }

    fn open_external(&mut self, _: &ClickEvent, _: &mut Window, _: &mut Context<Self>) {
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            let mut cmd = std::process::Command::new("cmd");
            cmd.args(["/c", "start", "", self.url.as_str()]);
            cmd.creation_flags(0x0800_0000);
            let _ = cmd.spawn();
        }
    }

    fn quit(&mut self, _: &ClickEvent, _: &mut Window, cx: &mut Context<Self>) {
        cx.quit();
    }
}

impl Render for AppView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let webview = self.webview.clone();
        v_flex()
            .size_full()
            .bg(rgb(0x0C0A08))
            .child(
                h_flex()
                    .h(px(48.))
                    .w_full()
                    .px_4()
                    .gap_2()
                    .items_center()
                    .justify_between()
                    .bg(rgb(0x16110C))
                    .border_b_1()
                    .border_color(rgb(0x3A2A1E))
                    .child(
                        h_flex()
                            .gap_2()
                            .items_center()
                            .child(div().size(px(8.)).rounded_full().bg(rgb(0x5DCAA5)))
                            .child(div().text_color(rgb(0xE8D8BC)).text_sm().child(TITLE))
                            .child(div().text_color(rgb(0x8A7C68)).text_xs().child("Rust 内核 · 原网页渲染"))
                            .child(div().text_color(rgb(0x6F8B8A)).text_xs().child("· 本机离线运行")),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .items_center()
                            .child(Button::new("reload").primary().label("刷新").on_click(cx.listener(Self::reload)))
                            .child(Button::new("browser").primary().label("浏览器打开").on_click(cx.listener(Self::open_external)))
                            .child(Button::new("quit").primary().label("退出").on_click(cx.listener(Self::quit))),
                    ),
            )
            .child(div().flex_1().w_full().child(webview.clone()))
    }
}

fn main() {
    // WebView2 子视图在 Windows 上必须先关掉 GPUI 的 DirectComposition，否则不绘制。
    #[cfg(target_os = "windows")]
    unsafe {
        std::env::set_var("GPUI_DISABLE_DIRECT_COMPOSITION", "true");
    }

    let root = project_root();
    let preferred = std::env::var("QX_PORT")
        .ok()
        .and_then(|s| s.parse::<u16>().ok())
        .unwrap_or(8765);

    // 端口被旧实例占用时顺延，保证同时开着旧桌面端也不会冲突。
    let port = (preferred..preferred + 8)
        .find_map(|p| http::spawn(root.clone(), p).ok())
        .expect("无法启动 Rust 本地服务（8765-8772 均被占用）");
    let url = Arc::new(format!("http://127.0.0.1:{port}/"));
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
