#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

//! 侨乡 · 江门 AI 智能讲解员 —— 桌面端
//!
//! 网页和 Python 服务都保持原样：这里只用 GPUI Kit 开一个原生窗口，
//! 通过官方 gpui-webview（Windows 走 WebView2）把 http://127.0.0.1:8765/
//! 原样嵌进来，另加一条原生顶栏做桌面端该有的事（状态、刷新、外部打开、退出）。
//!
//! 参考了 gpui-kit 官方 examples/webview：Windows 下必须关闭 DirectComposition，
//! 否则子 WebView 不会绘制。

mod server;

use std::{path::PathBuf, sync::Arc};

use gpui_kit::*;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{h_flex, v_flex};
use gpui_webview::WebView;

const TITLE: &str = "侨乡 · 江门 AI 智能讲解员";

fn project_root() -> PathBuf {
    // 1) 显式指定：项目目录被移动、或 exe 单独拷到别处时最可靠
    if let Ok(root) = std::env::var("QIAOXIANG_ROOT") {
        let p = PathBuf::from(root);
        if p.join("server").join("main.py").is_file() {
            return p;
        }
    }

    // 2/3) 从 exe 所在目录、当前工作目录逐级向上找 server/main.py。
    // 这样 exe 放在 desktop/target/release/、项目根目录、或由快捷方式带工作目录启动都能定位。
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        let mut dir = exe.parent().map(|p| p.to_path_buf());
        for _ in 0..6 {
            let d = match dir.clone() {
                Some(d) => d,
                None => break,
            };
            candidates.push(d.clone());
            dir = d.parent().map(|p| p.to_path_buf());
        }
    }
    if let Ok(cwd) = std::env::current_dir() {
        let mut dir = Some(cwd);
        for _ in 0..4 {
            let d = match dir.clone() {
                Some(d) => d,
                None => break,
            };
            candidates.push(d.clone());
            dir = d.parent().map(|p| p.to_path_buf());
        }
    }
    for d in &candidates {
        if d.join("server").join("main.py").is_file() {
            return d.clone();
        }
    }

    // 4) 开发期兜底：编译时的 desktop/ 目录
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."))
        .to_path_buf()
}

#[cfg(windows)]
fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(windows)]
fn alert(text: &str, icon: u32) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{MB_OK, MB_TOPMOST, MessageBoxW};
    unsafe {
        let body = wide(text);
        let title = wide(TITLE);
        MessageBoxW(
            std::ptr::null_mut(),
            body.as_ptr(),
            title.as_ptr(),
            MB_OK | icon | MB_TOPMOST,
        );
    }
}

#[cfg(not(windows))]
fn alert(text: &str, _icon: u32) {
    eprintln!("{TITLE}: {text}");
}

pub struct AppView {
    webview: Entity<WebView>,
    backend: Arc<server::Backend>,
}

impl AppView {
    fn new(webview: Entity<WebView>, backend: Arc<server::Backend>) -> Self {
        Self { webview, backend }
    }

    fn reload(&mut self, _: &ClickEvent, _: &mut Window, cx: &mut Context<Self>) {
        let url = self.backend.url.clone();
        self.webview.update(cx, |view, _| view.load_url(&url));
    }

    fn open_external(&mut self, _: &ClickEvent, _: &mut Window, _: &mut Context<Self>) {
        let url = self.backend.url.clone();
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            let mut cmd = std::process::Command::new("cmd");
            cmd.args(["/c", "start", "", &url]);
            cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
            let _ = cmd.spawn();
        }
        #[cfg(not(windows))]
        let _ = url;
    }

    fn quit(&mut self, _: &ClickEvent, _: &mut Window, cx: &mut Context<Self>) {
        cx.quit();
    }
}

impl Render for AppView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let webview = self.webview.clone();
        let state = if self.backend.reused {
            "已连接现有服务"
        } else {
            "本地服务已启动"
        };

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
                            .child(
                                div()
                                    .text_color(rgb(0xE8D8BC))
                                    .text_sm()
                                    .child(TITLE),
                            )
                            .child(
                                div()
                                    .text_color(rgb(0x8A7C68))
                                    .text_xs()
                                    .child(state),
                            )
                            .child(
                                div()
                                    .text_color(rgb(0x6F8B8A))
                                    .text_xs()
                                    .child("· 本机离线运行"),
                            ),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .items_center()
                            .child(
                                Button::new("reload")
                                    .primary()
                                    .label("刷新")
                                    .on_click(cx.listener(Self::reload)),
                            )
                            .child(
                                Button::new("browser")
                                    .primary()
                                    .label("浏览器打开")
                                    .on_click(cx.listener(Self::open_external)),
                            )
                            .child(
                                Button::new("quit")
                                    .primary()
                                    .label("退出")
                                    .on_click(cx.listener(Self::quit)),
                            ),
                    ),
            )
            .child(div().flex_1().w_full().child(webview.clone()))
    }
}

fn main() {
    // WebView2 子视图在 Windows 上必须先关掉 GPUI 的 DirectComposition
    #[cfg(target_os = "windows")]
    unsafe {
        std::env::set_var("GPUI_DISABLE_DIRECT_COMPOSITION", "true");
    }

    let root = project_root();
    server::log_line(&root, "桌面端启动");

    let _instance_lock = match server::acquire_instance_lock() {
        Some(lock) => lock,
        None => {
            server::log_line(&root, "检测到已有实例，退出");
            #[cfg(windows)]
            alert(
                "讲解员桌面端已经在运行了。\n请在任务栏里切换过去。",
                windows_sys::Win32::UI::WindowsAndMessaging::MB_ICONINFORMATION,
            );
            #[cfg(not(windows))]
            alert("讲解员桌面端已经在运行了。", 0);
            return;
        }
    };

    // 先拉起 Ollama，服务端启动时的 warmup 才能把模型读进显存
    server::ensure_ollama(&root);

    let backend = match server::Backend::start(&root) {
        Ok(b) => Arc::new(b),
        Err(e) => {
            server::log_line(&root, &format!("启动失败：{e}"));
            #[cfg(windows)]
            alert(
                &e,
                windows_sys::Win32::UI::WindowsAndMessaging::MB_ICONERROR,
            );
            #[cfg(not(windows))]
            alert(&e, 0);
            return;
        }
    };

    let url = backend.url.clone();
    let for_window = backend.clone();

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
                cx.new(|_| AppView::new(webview, for_window))
            })
            .expect("无法打开窗口");
        });

    // 窗口关闭 / run 返回后，backend 的 Drop 会回收我们拉起的 uvicorn
    server::log_line(&root, "桌面端退出");
}
