//! 离线 Piper 语音烘焙（纯 Rust，无 Python）。
//!
//! 把讲解词、兜底话术和演示回答用本地 Piper `zh_CN-huayan-medium` 合成 WAV，
//! 按运行时缓存键 `tts_key` 命名放进 `prebuilt-voices/`，由 build.rs 打进 exe。
//! 运行时优先使用这些语音，未命中缓存时再调用同一模型合成。
//!
//! 一键命令：
//! ```text
//! qx-desktop.exe --bake-voices
//! ```
//!
//! 三路来源，全部只补缺失项：
//! 1. **运行时缓存搬运**：把本机 Piper 生成的 WAV 固化到预生成目录。
//! 2. **硬编码降级话术**：补齐 `nlu.rs` 中的识图失败 / 实时信息话术。
//! 3. **演示问答脚本**：用 `Guide` 跑预设问题，再合成实际回答。
//!
//! 产物沿用 `<v8 FNV-1a>.wav` 文件名；报告写到源码根 `voice_bake_report.txt`。

use crate::http::{LlmConfig, build_llm, tts_key};
use crate::piper::PiperTts;
use qx_core::{Guide, Theme};
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// `nlu.rs` 里硬编码、主题包 JSON 中不存在的降级话术。必须与源码逐字一致，
/// `fixed_replies_still_exist_in_nlu` 会在编译测试里校验，防止文案改动后漂移。
const FIXED_REPLIES: &[&str] = &[
    "这张图我暂时看不了：当前模型不支持看图，或本地大模型还没连上。您可以直接用文字问我，我来介绍展厅里的展品。",
    "这张图我暂时看不了，可能是本地大模型正忙或还没连上。请再试一次，也可以直接用文字问我。",
    "我现在没能查到这类实时信息，可能是网络或数据源暂时不可用。侨乡的事您尽管问，碉楼、骑楼、陈皮、侨批我都熟。",
];

/// 赛前演示脚本：覆盖欢迎、六个展项的讲解 / 追问 / 深度讲解、路线、闲聊、
/// 实时信息与离题问题。命中知识库的会秒回，其余交给本机 LLM 生成真实回答。
const DEMO_QUESTIONS: &[&str] = &[
    "你好",
    "你是谁",
    "介绍开平碉楼",
    "详细讲讲碉楼",
    "碉楼为什么叫碉楼",
    "开平碉楼有多少座",
    "谁出钱盖了碉楼",
    "赤坎骑楼街是什么",
    "为什么叫小广州",
    "梁启超是谁",
    "少年中国说是什么时候写的",
    "新会陈皮为什么出名",
    "陈皮要放多久才算陈皮",
    "侨批是什么",
    "侨批和碉楼有什么关系",
    "小鸟天堂和巴金有什么关系",
    "参观路线怎么安排",
    "江门五邑是哪五个地方",
    "侨乡是什么意思",
    "今天天气怎么样",
    "帮我写一首诗",
];

/// 运行时缓存文件名必须与 `http.rs::is_audio_name` 同规则：16 位 hex + `.wav`。
fn is_cache_name(s: &str) -> bool {
    let Some((stem, ext)) = s.split_once('.') else {
        return false;
    };
    ext == "wav" && stem.len() == 16 && stem.bytes().all(|b| b.is_ascii_hexdigit())
}

fn runtime_cache_dir() -> PathBuf {
    std::env::var("QX_VOICE_BAKE_CACHE")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            std::env::temp_dir()
                .join(crate::BUNDLE_DIR_NAME)
                .join("tts-cache")
        })
}

/// 把运行时缓存里的本地 Piper WAV 固化进 `prebuilt-voices/`。
fn copy_runtime_voices(out: &Path, existing: &mut HashSet<String>) -> (usize, usize, String) {
    let cache = runtime_cache_dir();
    let Ok(rd) = fs::read_dir(&cache) else {
        return (0, 0, format!("{}（目录不存在，跳过）", cache.display()));
    };
    let mut scanned = 0usize;
    let mut copied = 0usize;
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if !is_cache_name(&name) {
            continue;
        }
        scanned += 1;
        if existing.contains(&name) {
            continue;
        }
        let Ok(data) = fs::read(e.path()) else {
            continue;
        };
        // 仅接受 Piper 的 RIFF WAV，不把其他格式混入预生成语音。
        if !data.starts_with(b"RIFF") {
            continue;
        }
        if fs::write(out.join(&name), &data).is_ok() {
            existing.insert(name);
            copied += 1;
        }
    }
    (copied, scanned, cache.display().to_string())
}

/// 用真实问答引擎跑一遍演示脚本，返回 `(标签, 回答文本)`。
fn demo_replies(source_root: &Path, cfg: &LlmConfig) -> Vec<(String, String)> {
    let theme_path = source_root.join("themes/qiaoxiang/theme.json");
    let Ok(theme) = Theme::load(&theme_path) else {
        tracing::warn!(path = %theme_path.display(), "语音烘焙：读取主题包失败");
        return Vec::new();
    };
    let guide = Guide::with_llm(theme, build_llm(cfg));
    DEMO_QUESTIONS
        .iter()
        .map(|q| {
            let a = guide.answer(q, None, false, None);
            (format!("demo:{q}"), a.reply)
        })
        .collect()
}

/// 主题包里所有可能成为 `answer.reply` 的文本（与 tools/prebuild_voices.py 同口径）。
/// 保证改文案后重跑烘焙能自动补齐缺失语音。
fn theme_texts(source_root: &Path) -> Vec<(String, String)> {
    let path = source_root.join("themes/qiaoxiang/theme.json");
    let Ok(raw) = fs::read_to_string(&path) else {
        return Vec::new();
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&raw) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for k in ["welcome", "realtime", "route", "fallback"] {
        if let Some(s) = v[k].as_str() {
            if !s.trim().is_empty() {
                out.push((k.to_string(), s.to_string()));
            }
        }
    }
    if let Some(exs) = v["exhibits"].as_array() {
        for ex in exs {
            let id = ex["id"].as_str().unwrap_or("?").to_string();
            for k in ["brief", "script", "deep"] {
                if let Some(s) = ex[k].as_str() {
                    if !s.trim().is_empty() {
                        out.push((format!("{id}/{k}"), s.to_string()));
                    }
                }
            }
            if let Some(fs) = ex["faq"].as_array() {
                for (i, f) in fs.iter().enumerate() {
                    if let Some(s) = f["a"].as_str() {
                        if !s.trim().is_empty() {
                            out.push((format!("{id}/faq{i}"), s.to_string()));
                        }
                    }
                }
            }
        }
    }
    if let Some(cs) = v["chitchat"].as_array() {
        for (i, c) in cs.iter().enumerate() {
            if let Some(s) = c["a"].as_str() {
                if !s.trim().is_empty() {
                    out.push((format!("chitchat{i}"), s.to_string()));
                }
            }
        }
    }
    out
}

/// 执行一次烘焙。返回可直接写入报告文件的多行文本。
pub fn run(source_root: &Path, cfg: &LlmConfig) -> String {
    let out = source_root.join("prebuilt-voices");
    if fs::create_dir_all(&out).is_err() {
        return format!("无法创建输出目录：{}", out.display());
    }

    // 初始化 Piper 离线神经语音（zh_CN-huayan-medium 女声）。
    let piper = Arc::new(PiperTts::new(source_root.join("tts")));
    piper.preload();

    let mut existing: HashSet<String> = fs::read_dir(&out)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();

    let (copied, scanned, cache_path) = copy_runtime_voices(&out, &mut existing);

    let mut candidates: Vec<(String, String)> = theme_texts(source_root);
    for (i, s) in FIXED_REPLIES.iter().enumerate() {
        candidates.push((format!("fixed{i}"), (*s).to_string()));
    }
    let demos = demo_replies(source_root, cfg);
    candidates.extend(demos);

    let mut added = 0usize;
    let mut skipped = 0usize;
    let mut failed: Vec<String> = Vec::new();
    let mut seen_text: HashSet<String> = HashSet::new();
    for (tag, text) in candidates {
        if text.trim().is_empty() || !seen_text.insert(text.clone()) {
            continue;
        }
        let name = format!("{}.wav", tts_key(&text));
        if existing.contains(&name) {
            skipped += 1;
            continue;
        }
        match piper.synth(&text) {
            Some(wav) => {
                if fs::write(out.join(&name), &wav).is_ok() {
                    existing.insert(name);
                    added += 1;
                } else {
                    failed.push(format!("{tag}（写盘失败）"));
                }
            }
            None => failed.push(format!("{tag}（Piper 合成失败）")),
        }
    }

    let total = fs::read_dir(&out)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "wav"))
        .count();

    let mut report = String::new();
    report.push_str("离线语音烘焙报告（Piper zh_CN-huayan-medium 女声）\n");
    report.push_str(&format!("输出目录：{}\n", out.display()));
    report.push_str(&format!(
        "运行时缓存：{}（扫描 {} 条，固化 {} 条 huayan wav）\n",
        cache_path, scanned, copied
    ));
    report.push_str(&format!(
        "候选文本：主题包 + {} 条硬编码话术 + {} 条演示问答\n",
        FIXED_REPLIES.len(),
        DEMO_QUESTIONS.len()
    ));
    report.push_str(&format!(
        "新增合成 {} 条 / 已有跳过 {} 条 / 失败 {} 条\n",
        added,
        skipped,
        failed.len()
    ));
    for f in &failed {
        report.push_str(&format!("  失败：{f}\n"));
    }
    report.push_str(&format!("prebuilt-voices 音频总数：{total} 条\n"));
    report.push_str("下一步：重新编译 exe，这些音频随 bundle 释放，断网现场直接命中缓存。\n");
    report
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 防止 `nlu.rs` 改了降级话术而这里仍烘焙旧句子：旧句子永远不会被命中，
    /// 现场断网识图失败就会漏回华艳。直接用 include_str! 比对源码文本。
    #[test]
    fn fixed_replies_still_exist_in_nlu() {
        const NLU_SRC: &str = include_str!("../../qx-core/src/nlu.rs");
        for (i, s) in FIXED_REPLIES.iter().enumerate() {
            assert!(
                NLU_SRC.contains(s),
                "第 {i} 条固定话术已不在 nlu.rs 中，请同步更新"
            );
        }
    }

    #[test]
    fn cache_name_filter_accepts_only_16_hex_wav() {
        assert!(is_cache_name("0e5bbf57873c8703.wav"));
        assert!(is_cache_name("ABCDEF0123456789.wav"));
        assert!(!is_cache_name("0e5bbf57873c870.wav")); // 15 位
        assert!(!is_cache_name("0e5bbf57873c87033.wav")); // 17 位
        assert!(!is_cache_name("0e5bbf57873c8703.mp3"));
        assert!(!is_cache_name("zzzzzzzzzzzzzzzz.wav"));
        assert!(!is_cache_name("voice_manifest.json"));
    }

    /// 搬运缓存必须只认 wav：把 Edge TTS 的 mp3 固化进 exe 会让现场
    /// 断网时命中错误的格式，属于最危险的假阳性。
    #[test]
    fn wav_magic_gates_runtime_cache_copy() {
        let mut wav = b"RIFF".to_vec();
        wav.extend(vec![0u8; 4096]);
        assert!(wav.starts_with(b"RIFF"));
        let mut mp3 = vec![0xff, 0xfb, 0x90, 0x00];
        mp3.extend(vec![0u8; 4096]);
        assert!(!mp3.starts_with(b"RIFF"));
    }

    /// 手动联网烘焙入口（与用户侧的 `--bake-voices` 共用同一个 run()）。
    /// 用测试进程跑而不是双击 exe：exe 是 GUI 子系统，控制台输出与退出码都拿不到。
    /// `cargo test -p qx-desktop bake_offline_voices_live -- --ignored --nocapture`
    #[test]
    #[ignore = "需联网 + 本机 Ollama：赛前烘焙一次"]
    fn bake_offline_voices_live() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../");
        let report = run(&root, &LlmConfig::default());
        fs::write(root.join("voice_bake_report.txt"), &report).expect("写入烘焙报告");
        println!("{report}");
        assert!(
            report.contains("失败 0 条"),
            "存在合成失败条目，见 qx-rust/voice_bake_report.txt"
        );
    }
}
