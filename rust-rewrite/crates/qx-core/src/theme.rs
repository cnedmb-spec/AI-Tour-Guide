//! 主题包结构与加载。
//!
//! 直接消费现有 `themes/qiaoxiang/theme.json`，不改动数据文件——重写过程中
//! 内容层保持单一来源，避免两套讲解词漂移（对应原项目 DEF-14 的结构校验）。

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

#[derive(Debug, thiserror::Error)]
pub enum ThemeError {
    #[error("读取主题包失败: {0}")]
    Io(#[from] std::io::Error),
    #[error("解析主题包失败: {0}")]
    Json(#[from] serde_json::Error),
    #[error("主题包结构不完整: {0}")]
    Invalid(String),
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct Assistant {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub title: String,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct Palette {
    #[serde(default)]
    pub bg: String,
    #[serde(default)]
    pub fog: String,
    #[serde(default)]
    pub floor: String,
    #[serde(default)]
    pub accent: String,
    #[serde(default)]
    pub accent2: String,
    #[serde(default)]
    pub key: String,
    #[serde(default)]
    pub fill: String,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct Faq {
    #[serde(default)]
    pub q: String,
    #[serde(default)]
    pub a: String,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct Exhibit {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub prop: String,
    #[serde(default)]
    pub pos: Vec<f64>,
    #[serde(default)]
    pub aliases: Vec<String>,
    #[serde(default)]
    pub keywords: Vec<String>,
    #[serde(default)]
    pub brief: String,
    #[serde(default)]
    pub script: String,
    #[serde(default)]
    pub deep: String,
    #[serde(default)]
    pub faq: Vec<Faq>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default, rename = "modelScale")]
    pub model_scale: Option<f64>,
    // 以下字段当前版本只透传，渲染层接入时再细化类型
    #[serde(default)]
    pub plaque: Option<serde_json::Value>,
    #[serde(default)]
    pub shift: Option<serde_json::Value>,
    #[serde(default)]
    pub materials: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct Chitchat {
    #[serde(default)]
    pub k: Vec<String>,
    #[serde(default)]
    pub a: String,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct ExhibitStyle {
    #[serde(default)]
    pub emotion: String,
    #[serde(default)]
    pub gesture: String,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct Theme {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub short: String,
    #[serde(default)]
    pub scene: serde_json::Value,
    #[serde(default)]
    pub assistant: Assistant,
    #[serde(default)]
    pub palette: Palette,
    #[serde(default)]
    pub welcome: String,
    #[serde(default)]
    pub route: String,
    #[serde(default)]
    pub fallback: String,
    #[serde(default)]
    pub realtime: String,
    #[serde(default)]
    pub exhibit_styles: HashMap<String, ExhibitStyle>,
    #[serde(default)]
    pub exhibits: Vec<Exhibit>,
    #[serde(default)]
    pub chitchat: Vec<Chitchat>,
}

impl Theme {
    pub fn load(path: impl AsRef<Path>) -> Result<Self, ThemeError> {
        let raw = std::fs::read_to_string(path)?;
        Ok(serde_json::from_str(&raw)?)
    }

    /// 对应原前端 `bootFailed` 与后端主题包校验：缺关键字段时尽早失败，
    /// 不要等到渲染层遇到 `undefined` 才崩。
    pub fn validate(&self) -> Result<(), ThemeError> {
        if self.id.is_empty() {
            return Err(ThemeError::Invalid("缺少 id".into()));
        }
        if self.assistant.name.is_empty() {
            return Err(ThemeError::Invalid("缺少 assistant.name".into()));
        }
        if self.palette.accent.is_empty() {
            return Err(ThemeError::Invalid("缺少 palette.accent".into()));
        }
        if self.exhibits.is_empty() {
            return Err(ThemeError::Invalid("exhibits 为空".into()));
        }
        for e in &self.exhibits {
            if e.id.is_empty() || e.name.is_empty() {
                return Err(ThemeError::Invalid(format!("展品缺少 id/name: {:?}", e.id)));
            }
            if e.script.is_empty() {
                return Err(ThemeError::Invalid(format!("展品 {} 缺少 script", e.id)));
            }
        }
        Ok(())
    }

    pub fn exhibit(&self, id: &str) -> Option<&Exhibit> {
        self.exhibits.iter().find(|e| e.id == id)
    }

    /// 对齐 `/api/stats` 的 `kb_answers` 口径：sum(len(faq) + 2)。
    pub fn kb_answers(&self) -> usize {
        self.exhibits.iter().map(|e| e.faq.len() + 2).sum()
    }
}
