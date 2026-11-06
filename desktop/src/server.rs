//! 本地服务生命周期管理（桌面端专用）。
//!
//! 设计前提：网页与 Python 服务都保持原样，桌面端只是一个外壳。
//! 这里负责「确认服务在线 / 必要时拉起 / 退出时收尾」，不做任何业务逻辑。

use std::{
    env, fs,
    io::{Read, Write},
    net::{SocketAddr, TcpListener, TcpStream},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

pub const PORT: u16 = 8765;
const OLLAMA_PORT: u16 = 11434;
const READY_TIMEOUT: Duration = Duration::from_secs(90);

/// 允许用 QIAOXIANG_PORT 把桌面端指向另一个端口（调试、或 8765 被占用时）。
pub fn port() -> u16 {
    env::var("QIAOXIANG_PORT")
        .ok()
        .and_then(|v| v.parse::<u16>().ok())
        .filter(|p| *p > 0)
        .unwrap_or(PORT)
}

/// 绑定一个本机端口作为实例锁：第二个实例会绑定失败。
/// 进程退出时监听自动释放，不留锁文件。
pub struct InstanceLock(#[allow(dead_code)] TcpListener);

pub fn acquire_instance_lock() -> Option<InstanceLock> {
    let lock_port = port().saturating_add(1);
    TcpListener::bind(("127.0.0.1", lock_port))
        .ok()
        .map(InstanceLock)
}

fn addr(port: u16) -> SocketAddr {
    SocketAddr::from(([127, 0, 0, 1], port))
}

fn http_get(port: u16, path: &str, timeout: Duration) -> Option<String> {
    let mut stream = TcpStream::connect_timeout(&addr(port), timeout).ok()?;
    let _ = stream.set_read_timeout(Some(timeout));
    let _ = stream.set_write_timeout(Some(timeout));
    let req = format!(
        "GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\nAccept: */*\r\n\r\n"
    );
    stream.write_all(req.as_bytes()).ok()?;
    let mut buf = Vec::new();
    stream.read_to_end(&mut buf).ok()?;
    let text = String::from_utf8_lossy(&buf);
    if !text.starts_with("HTTP/1.1 200") && !text.starts_with("HTTP/1.0 200") {
        return None;
    }
    let body = text.split_once("\r\n\r\n")?.1;
    Some(body.to_string())
}

/// 真正调一次 /api/stats：端口通且返回的是「我们的服务」才算就绪，
/// 避免把占用 8765 的其它程序误判成已启动。
pub fn service_ready(port: u16) -> bool {
    matches!(
        http_get(port, "/api/stats", Duration::from_millis(900)),
        Some(body) if body.contains("\"theme\"") && body.contains("kb_exhibits")
    )
}

fn port_busy(port: u16) -> bool {
    TcpStream::connect_timeout(&addr(port), Duration::from_millis(250)).is_ok()
}

pub fn log_line(root: &Path, message: &str) {
    let path = root.join("server").join("desktop-launcher.log");
    if let Ok(mut f) = fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(f, "[{}] {}", timestamp(), message);
    }
}

fn timestamp() -> String {
    // 不引入 chrono，用系统时间的秒数做一个足够排查问题的时间戳
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("{}", secs)
}

fn python_has_backend_deps(python: &Path) -> bool {
    Command::new(python)
        .args(["-c", "import fastapi, uvicorn, sherpa_onnx, piper, jieba, pypinyin"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// 找一台装好了服务端依赖的 Python。
/// PATH 里可能混着别的解释器（例如编辑器自带的），所以要逐个校验。
fn find_python() -> Option<PathBuf> {
    if let Ok(p) = env::var("QIAOXIANG_PYTHON") {
        let path = PathBuf::from(p);
        if path.is_file() && python_has_backend_deps(&path) {
            return Some(path);
        }
    }
    if let Ok(local) = env::var("LOCALAPPDATA") {
        for ver in ["Python311", "Python312", "Python313", "Python310"] {
            let path = PathBuf::from(&local)
                .join("Programs")
                .join("Python")
                .join(ver)
                .join("python.exe");
            if path.is_file() && python_has_backend_deps(&path) {
                return Some(path);
            }
        }
    }
    if let Ok(out) = Command::new("cmd").args(["/c", "where python"]).output() {
        for line in String::from_utf8_lossy(&out.stdout).lines() {
            let line = line.trim();
            if line.is_empty() || line.contains("WindowsApps") {
                continue;
            }
            let path = PathBuf::from(line);
            if path.is_file() && python_has_backend_deps(&path) {
                return Some(path);
            }
        }
    }
    None
}

/// 本地大模型没起就顺手拉起；没装 Ollama 则静默跳过，讲解员会回落到知识库。
pub fn ensure_ollama(root: &Path) {
    if port_busy(OLLAMA_PORT) {
        return;
    }
    let exe = env::var("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|_| root.to_path_buf())
        .join("Programs")
        .join("Ollama")
        .join("ollama app.exe");
    if exe.is_file() {
        match Command::new(&exe).spawn() {
            Ok(_) => log_line(root, "Ollama 未运行，已拉起托盘程序"),
            Err(e) => log_line(root, &format!("Ollama 拉起失败：{e}")),
        }
    }
}

pub struct Backend {
    child: Option<Child>,
    pub url: String,
    pub reused: bool,
}

impl Backend {
    /// 复用已有服务；没有才自己起一个。返回的 Backend 被 drop 时会自动收尾。
    pub fn start(root: &Path) -> Result<Self, String> {
        let port = port();
        let url = format!("http://127.0.0.1:{port}/");

        if service_ready(port) {
            log_line(root, &format!("检测到已运行的讲解服务（{port}），直接复用"));
            return Ok(Self { child: None, url, reused: true });
        }
        if port_busy(port) {
            return Err(format!(
                "端口 {port} 已被其它程序占用，桌面端无法启动。\n请结束占用该端口的进程后重试。"
            ));
        }

        let python = find_python().ok_or_else(|| {
            "没找到装有服务端依赖的 Python。\n请确认已安装依赖，或设置环境变量 QIAOXIANG_PYTHON 指向 python.exe。".to_string()
        })?;
        log_line(root, &format!("使用 Python：{}", python.display()));

        let log_path = root.join("server").join("desktop-server.log");
        let log = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)
            .map_err(|e| format!("无法写日志 {}：{e}", log_path.display()))?;

        let port_arg = port.to_string();
        let mut cmd = Command::new(&python);
        cmd.args([
            "-m", "uvicorn", "main:app", "--host", "127.0.0.1", "--port", &port_arg,
            "--log-level", "info",
        ])
        .current_dir(root.join("server"))
        .stdin(Stdio::null())
        .stdout(Stdio::from(log.try_clone().map_err(|e| e.to_string())?))
        .stderr(Stdio::from(log));
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            // CREATE_NO_WINDOW：桌面端不弹黑色控制台
            cmd.creation_flags(0x0800_0000);
        }

        let mut child = cmd.spawn().map_err(|e| format!("启动讲解服务失败：{e}"))?;
        log_line(root, &format!("uvicorn 已启动，pid={}", child.id()));

        let deadline = Instant::now() + READY_TIMEOUT;
        loop {
            if service_ready(port) {
                log_line(root, &format!("讲解服务健康检查通过（{port}）"));
                return Ok(Self { child: Some(child), url, reused: false });
            }
            if let Ok(Some(status)) = child.try_wait() {
                let _ = child.kill();
                return Err(format!(
                    "讲解服务启动后立即退出（{status}）。\n请查看 server\\desktop-server.log"
                ));
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                return Err(format!(
                    "讲解服务在 90 秒内未就绪。\n请查看 server\\desktop-server.log"
                ));
            }
            std::thread::sleep(Duration::from_millis(300));
        }
    }
}

impl Drop for Backend {
    fn drop(&mut self) {
        // 只回收自己拉起的服务；复用外部服务时绝不误杀（网页版可能正在用）。
        if let Some(child) = self.child.as_mut() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}
