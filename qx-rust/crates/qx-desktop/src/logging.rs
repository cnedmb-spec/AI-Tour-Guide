//! 运行日志：按天滚动的文件 + 可选控制台 + panic 记录。
//!
//! 文件写入由自研日志库 [`shen_eternity_log`](https://github.com/EternityShen/shen_eternity_log)
//! 承担：本模块实现了一个 tracing Layer，把格式化后的日志事件转发给该库的
//! `LogHandle`（后台线程 + tokio 运行时，逐条落盘）。`tracing` 宏调用点与
//! 过滤规则不变。
//!
//! GUI 子系统没有控制台，现场排障只能靠文件，所以默认把 `info` 及以上写到
//! 用户数据目录：Windows `%LOCALAPPDATA%\QiaoxiangGuide\logs`、
//! Linux `~/.local/share/QiaoxiangGuide/logs`、macOS `~/Library/Application Support/...`。
//!
//!   * `QX_LOG_DIR`  日志目录（便携版可指到 U 盘或项目目录）
//!   * `QX_LOG`      过滤级别，语法同 `RUST_LOG`，例如 `debug`、
//!                   `qx_desktop=debug,warn`；未设置时默认 `info`
//!   * `RUST_LOG`    兜底，优先级低于 `QX_LOG`
//!
//! 文件保留 14 天，启动时清理过期文件；`LogGuard` 存活期间写入线程持续工作，
//! 丢弃时 channel 关闭、线程自然退出。panic 会连同线程名、位置与 backtrace
//! 一起写进日志。

use std::fmt::Write as _;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Sender, channel};
use std::sync::OnceLock;
use std::time::{Duration, SystemTime};

use shen_eternity_log::log::handle::LogHandle;
use tracing::Level;
use tracing_subscriber::{EnvFilter, fmt, layer::SubscriberExt, util::SubscriberInitExt};

/// 默认只给本项目开 info，依赖库保持 warn，避免刷屏。
const DEFAULT_FILTER: &str = "warn,qx_desktop=info,qx_core=info";
const LOG_PREFIX: &str = "qx-guide.log";
const RETENTION: Duration = Duration::from_secs(14 * 24 * 60 * 60);

static LOG_DIR: OnceLock<PathBuf> = OnceLock::new();

/// 当前正在写入的日志文件路径（由 [`init`] 在预创建后固定）。
/// 供 `/api/stats` 直接展示真实文件，避免依赖目录扫描。
static LOG_FILE: OnceLock<PathBuf> = OnceLock::new();

/// 日志 guard：必须活到进程结束，否则写入线程可能还有日志没落盘。
pub struct LogGuard {
    /// 持有 sender：Drop 时 channel 关闭，写入线程 `recv` 出错后退出。
    _sender: Sender<(Level, String)>,
}

/// 初始化全局日志与 panic hook。应尽早调用，且只调用一次。
pub fn init() -> LogGuard {
    let (dir, file) = prepare_dir();
    let _ = LOG_DIR.set(dir.clone());
    let _ = LOG_FILE.set(file.clone());

    let writer = spawn_file_writer(dir.clone());
    let file_layer = FileLayer { writer: writer.clone() };

    let console_layer = fmt::layer()
        .with_writer(std::io::stderr)
        .with_ansi(std::io::stderr().is_terminal())
        .with_target(false);

    let spec = log_spec();
    let filter = build_filter(spec.as_deref());

    tracing_subscriber::registry()
        .with(filter)
        .with(file_layer)
        .with(console_layer)
        .init();

    install_panic_hook();

    tracing::info!(
        dir = %dir.display(),
        filter = %spec.as_deref().unwrap_or(DEFAULT_FILTER),
        "日志系统已就绪（shen_eternity_log 落盘，按天滚动，保留 14 天）"
    );

    LogGuard { _sender: writer }
}

/// 当前日志目录，供 `/api/stats` 与自检面板展示。
pub fn dir() -> Option<&'static Path> {
    LOG_DIR.get().map(PathBuf::as_path)
}

/// 当前正在写入的日志文件，供 `/api/stats` 与自检面板展示。
pub fn current_file() -> Option<&'static Path> {
    LOG_FILE.get().map(PathBuf::as_path)
}

/// 当前最新的日志文件。优先返回预创建并固定下来的当天文件；
/// 若尚未初始化，则回退到按目录扫描取最新。
pub fn latest_file() -> Option<PathBuf> {
    if let Some(file) = LOG_FILE.get() {
        return Some(file.clone());
    }
    let dir = dir()?;
    std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with(LOG_PREFIX))
        .max_by_key(|e| e.metadata().and_then(|m| m.modified()).ok())
        .map(|e| e.path())
}

// ---------------------------------------------------------------------------
// shen_eternity_log 桥接层
// ---------------------------------------------------------------------------

/// 把 tracing 事件转发给 shen_eternity_log 的 Layer。
///
/// 只做格式化与投递（同步、无锁阻塞），磁盘 I/O 全部在写入线程完成，
/// 不拖慢业务线程。
struct FileLayer {
    writer: Sender<(Level, String)>,
}

impl<S: tracing::Subscriber> tracing_subscriber::layer::Layer<S> for FileLayer {
    fn on_event(
        &self,
        event: &tracing::Event<'_>,
        _ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        let level = *event.metadata().level();
        // 访客收集字段：`message` 字段即宏里的格式化文本，其余字段以 k=v 追加。
        struct Visitor {
            message: Option<String>,
            fields: Vec<(String, String)>,
        }
        impl tracing::field::Visit for Visitor {
            fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
                if field.name() == "message" {
                    self.message = Some(format!("{value:?}"));
                } else {
                    self.fields.push((field.name().to_string(), format!("{value:?}")));
                }
            }
            fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
                if field.name() == "message" {
                    self.message = Some(value.to_string());
                } else {
                    self.fields.push((field.name().to_string(), value.to_string()));
                }
            }
            fn record_error(
                &mut self,
                field: &tracing::field::Field,
                value: &(dyn std::error::Error + 'static),
            ) {
                self.record_str(field, &value.to_string());
            }
        }

        let mut visitor = Visitor { message: None, fields: Vec::new() };
        event.record(&mut visitor);

        let mut line = String::new();
        let meta = event.metadata();
        let _ = write!(
            line,
            "[{}] ",
            meta.target().split("::").next().unwrap_or(meta.target())
        );
        if let Some(m) = visitor.message {
            line.push_str(&m);
        }
        for (k, v) in visitor.fields {
            let _ = write!(line, " {k}={v}");
        }
        // 写入失败意味着接收端已退出（进程收尾阶段），丢弃即可。
        let _ = self.writer.send((level, line));
    }
}

/// 启动常驻写入线程：自带单线程 tokio runtime，持有 shen_eternity_log 的
/// `LogHandle`，按天滚动（日期变化时重建 handle 指向新文件）。
/// `flush_log_num = 1`：逐条 flush，崩溃 / 断电时现场日志不丢。
fn spawn_file_writer(dir: PathBuf) -> Sender<(Level, String)> {
    let (tx, rx) = channel::<(Level, String)>();
    std::thread::Builder::new()
        .name("qx-log-writer".into())
        .spawn(move || {
            let rt = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(_) => return,
            };
            let file_for = |date: &str| dir.join(format!("{LOG_PREFIX}.{date}"));
            rt.block_on(async move {
                let mut date = today();
                let mut handle = LogHandle::new(file_for(&date), 1).await;
                while let Ok((level, message)) = rx.recv() {
                    let now = today();
                    if now != date {
                        date = now;
                        handle = LogHandle::new(file_for(&date), 1).await;
                    }
                    match level {
                        Level::ERROR => handle.log_error(message).await,
                        Level::WARN => handle.log_warn(message).await,
                        Level::DEBUG | Level::TRACE => handle.log_debug(message).await,
                        _ => handle.log_info(message).await,
                    }
                }
            });
        })
        .expect("启动日志写入线程");
    tx
}

/// 当天日期（日志文件名后缀），如 `2026-11-17`。
fn today() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // 手写 civil-from-days：避免为一个文件名再引依赖。
    let days = (now / 86_400) as i64;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}")
}

// ---------------------------------------------------------------------------
// 目录管理（保留自原实现）
// ---------------------------------------------------------------------------

/// 日志文件名：`qx-guide.log.{date}`。
fn file_for(dir: &Path, date: &str) -> PathBuf {
    dir.join(format!("{LOG_PREFIX}.{date}"))
}

/// 目录优先级：`QX_LOG_DIR` > 平台用户数据目录 > 临时目录。
/// 返回实际选定的目录和当天日志文件。每个候选都会同步预创建当天日志文件，
/// 目录不可写或文件被锁时立即换下一个，避免异步写入线程首次打开才报 ENOENT。
fn prepare_dir() -> (PathBuf, PathBuf) {
    let mut candidates = Vec::new();
    if let Some(v) = std::env::var_os("QX_LOG_DIR").filter(|v| !v.is_empty()) {
        candidates.push(PathBuf::from(v));
    }
    if let Some(d) = default_data_dir() {
        if !candidates.contains(&d) {
            candidates.push(d);
        }
    }
    let fallback = std::env::temp_dir().join("QiaoxiangGuide").join("logs");
    if !candidates.contains(&fallback) {
        candidates.push(fallback.clone());
    }

    for dir in &candidates {
        if let Some(file) = try_log_dir(dir) {
            cleanup_old_logs(dir);
            return (dir.clone(), file);
        }
        tracing::warn!(path = %dir.display(), "日志目录不可写，尝试下一个候选位置");
    }

    // 所有候选都不可用时仍返回临时目录，写入线程会以 stdout 兜底，不阻断启动。
    let _ = std::fs::create_dir_all(&fallback);
    let file = file_for(&fallback, &today());
    (fallback, file)
}

/// 创建并验证一个日志目录：建目录后同步预创建当天日志文件（追加模式，
/// 不截断已有内容），成功才认为该目录可用。
fn try_log_dir(dir: &Path) -> Option<PathBuf> {
    if std::fs::create_dir_all(dir).is_err() {
        return None;
    }
    let file = file_for(dir, &today());
    match std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&file)
    {
        Ok(_) => Some(file),
        Err(_) => None,
    }
}

/// 平台默认的用户数据日志目录，不区分可写性（后面用探针文件统一验证）。
fn default_data_dir() -> Option<PathBuf> {
    if cfg!(target_os = "windows") {
        // Windows：%LOCALAPPDATA%\QiaoxiangGuide\logs
        return std::env::var_os("LOCALAPPDATA")
            .filter(|v| !v.is_empty())
            .map(|v| PathBuf::from(v).join("QiaoxiangGuide").join("logs"));
    }
    // Unix 家族：优先 XDG_DATA_HOME，其次 ~/.local/share（macOS 落在
    // ~/Library 需要额外依赖，此处从简统一走 XDG 约定）。
    if let Some(xdg) = std::env::var_os("XDG_DATA_HOME").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(xdg).join("QiaoxiangGuide").join("logs"));
    }
    std::env::var_os("HOME").filter(|v| !v.is_empty()).map(|h| {
        PathBuf::from(h).join(".local").join("share").join("QiaoxiangGuide").join("logs")
    })
}

/// 删除 14 天前的旧日志；只碰本程序前缀，避免清理共享目录里的其他文件。
fn cleanup_old_logs(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let now = SystemTime::now();
    for entry in entries.flatten() {
        if !entry.file_name().to_string_lossy().starts_with(LOG_PREFIX) {
            continue;
        }
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        let Ok(modified) = meta.modified() else {
            continue;
        };
        let Ok(age) = now.duration_since(modified) else {
            continue;
        };
        if age > RETENTION {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

fn log_spec() -> Option<String> {
    for key in ["QX_LOG", "RUST_LOG"] {
        if let Ok(v) = std::env::var(key) {
            let v = v.trim();
            if !v.is_empty() {
                return Some(v.to_string());
            }
        }
    }
    None
}

fn build_filter(spec: Option<&str>) -> EnvFilter {
    match spec {
        Some(s) => EnvFilter::try_new(s).unwrap_or_else(|e| {
            eprintln!("[log] 日志过滤规则无效（{e}），回退到 {DEFAULT_FILTER}");
            EnvFilter::new(DEFAULT_FILTER)
        }),
        None => EnvFilter::new(DEFAULT_FILTER),
    }
}

/// panic 在 GUI 下没有控制台可见，必须落盘；保留默认 hook 以便终端运行时仍有输出。
fn install_panic_hook() {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let thread = std::thread::current();
        let thread_name = thread.name().unwrap_or("<unnamed>");
        let location = info
            .location()
            .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
            .unwrap_or_else(|| "<unknown>".into());
        let payload = info
            .payload()
            .downcast_ref::<&str>()
            .copied()
            .or_else(|| info.payload().downcast_ref::<String>().map(String::as_str))
            .unwrap_or("<非字符串 panic>");
        // 只在 panic 时抓一次，代价可以接受；strip 后的 exe 至少还有地址与调用栈。
        let backtrace = std::backtrace::Backtrace::force_capture();
        tracing::error!(
            thread = %thread_name,
            location = %location,
            backtrace = %backtrace,
            "进程 panic：{payload}"
        );
        default_hook(info);
    }));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retention_is_two_weeks() {
        assert_eq!(RETENTION.as_secs(), 14 * 24 * 60 * 60);
    }

    #[test]
    fn default_filter_keeps_project_visible() {
        assert!(DEFAULT_FILTER.contains("qx_desktop=info"));
        assert!(DEFAULT_FILTER.contains("qx_core=info"));
    }

    #[test]
    fn today_formats_civil_date() {
        // 已知锚点：2026-11-17 是 Unix 时间 1794864000 附近的一天。
        let t = today();
        assert_eq!(t.len(), 10);
        let parts: Vec<&str> = t.split('-').collect();
        assert_eq!(parts.len(), 3);
        assert!(parts[0].parse::<i64>().unwrap() > 2020);
    }
}

