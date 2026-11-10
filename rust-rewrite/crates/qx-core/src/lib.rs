//! `qx-core`：侨乡·江门 AI 智能讲解员的**渲染无关核心层**。
//!
//! 阶段一只移植不依赖任何 UI / 3D 框架的部分：主题包、知识库路由、
//! 本地大模型客户端、指标。无论最终界面选 gpui-kit、Bevy 还是继续
//! WebView2，这一层都可以直接复用，且行为可用 `cargo test` 与现有
//! Python 实现逐条对照。
//!
//! 明确不在本层：`3D 场景 / VRM 数字人 / ASR / TTS / 音频口型 / HTTP 服务`。

pub mod camera;
pub mod llm;
pub mod metrics;
pub mod nlu;
pub mod theme;

pub use camera::{CameraPath, V3};
pub use llm::{LlmClient, Message, NoLlm, OllamaClient};
pub use metrics::{Metrics, MetricsSnapshot};
pub use nlu::{Answer, Guide};
pub use theme::{Theme, ThemeError};
