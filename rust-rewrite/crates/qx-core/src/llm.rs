//! 本地大模型客户端。
//!
//! 原 Python 版用 urllib 调 Ollama `/api/chat`。Rust 版刻意不引入
//! tokio/reqwest 整条异步链：桌面应用里 LLM 调用本就应放在后台线程，
//! 一个阻塞式 HTTP/1.1 客户端即可，依赖更少、启动更快（优化点）。

use serde::{Deserialize, Serialize};
use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: String,
    pub content: String,
}

impl Message {
    pub fn system(content: impl Into<String>) -> Self {
        Self { role: "system".into(), content: content.into() }
    }
    pub fn user(content: impl Into<String>) -> Self {
        Self { role: "user".into(), content: content.into() }
    }
}

pub trait LlmClient: Send + Sync {
    fn chat(
        &self,
        messages: &[Message],
        temperature: f32,
        num_predict: u32,
    ) -> Result<String, String>;
    fn available(&self) -> bool {
        true
    }
}

/// 无大模型时的占位实现：断网/Ollama 未安装也能完整演示知识库（对齐原项目降级设计）。
pub struct NoLlm;

impl LlmClient for NoLlm {
    fn chat(&self, _: &[Message], _: f32, _: u32) -> Result<String, String> {
        Err("LLM 未配置".into())
    }
    fn available(&self) -> bool {
        false
    }
}

#[derive(Debug, Clone)]
pub struct OllamaClient {
    pub host: String,
    pub model: String,
    pub timeout: Duration,
}

impl Default for OllamaClient {
    fn default() -> Self {
        Self {
            host: "http://127.0.0.1:11434".into(),
            model: "qwen3.5:4b".into(),
            timeout: Duration::from_secs(45),
        }
    }
}

impl OllamaClient {
    fn authority(&self) -> String {
        self.host
            .trim_start_matches("http://")
            .trim_start_matches("https://")
            .trim_end_matches('/')
            .to_string()
    }
}

impl LlmClient for OllamaClient {
    fn chat(
        &self,
        messages: &[Message],
        temperature: f32,
        num_predict: u32,
    ) -> Result<String, String> {
        let authority = self.authority();
        let addr = authority
            .to_socket_addrs()
            .map_err(|e| format!("解析 Ollama 地址失败: {e}"))?
            .next()
            .ok_or_else(|| "Ollama 地址无解析结果".to_string())?;

        let body = serde_json::json!({
            "model": self.model,
            "messages": messages,
            "stream": false,
            "think": false,
            // 模型常驻显存：否则 Ollama 默认 5 分钟后卸载，下一次提问要重新
            // 冷加载（数秒~十几秒），用户感知为「文字生成很慢」。
            "keep_alive": -1,
            "options": { "temperature": temperature, "num_predict": num_predict }
        })
        .to_string();

        let mut stream = TcpStream::connect_timeout(&addr, self.timeout)
            .map_err(|e| format!("连接 Ollama 失败: {e}"))?;
        stream
            .set_read_timeout(Some(self.timeout))
            .and_then(|_| stream.set_write_timeout(Some(self.timeout)))
            .map_err(|e| format!("设置超时失败: {e}"))?;

        write!(
            stream,
            "POST /api/chat HTTP/1.1\r\nHost: {authority}\r\n\
             Content-Type: application/json\r\nContent-Length: {}\r\n\
             Connection: close\r\n\r\n{body}",
            body.as_bytes().len()
        )
        .map_err(|e| format!("写入请求失败: {e}"))?;
        stream.flush().map_err(|e| format!("刷新请求失败: {e}"))?;

        let mut raw = Vec::new();
        stream
            .read_to_end(&mut raw)
            .map_err(|e| format!("读取响应失败: {e}"))?;
        let text = String::from_utf8_lossy(&raw);
        let split = text
            .find("\r\n\r\n")
            .ok_or_else(|| "Ollama 响应缺少 HTTP 头分隔".to_string())?;
        let payload = &text[split + 4..];
        let value: serde_json::Value =
            serde_json::from_str(payload).map_err(|e| format!("解析响应失败: {e}"))?;
        Ok(value["message"]["content"]
            .as_str()
            .unwrap_or("")
            .trim()
            .to_string())
    }
}
