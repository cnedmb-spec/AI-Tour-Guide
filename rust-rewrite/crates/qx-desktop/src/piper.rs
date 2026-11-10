//! 离线神经语音合成：Piper VITS（zh_CN-huayan 女声）+ espeak-ng 音素化。
//!
//! 这是「真实女孩子声音」的关键：替换掉 Windows System.Speech 的机械拼接音。
//! 与原 Python 版一致的技术栈（Piper + onnxruntime），但完全用 Rust 驱动，不依赖
//! Python：
//!   * 音素化：`espeakbridge.pyd`（piper 1.8 轮子里的 espeak-ng 静态链接库，
//!     已验证导出 espeak_Initialize / espeak_SetVoiceByName / espeak_TextToPhonemes）；
//!   * 声学模型：`zh_CN-huayan-medium.onnx`（原 `server/models/piper` 同款，22050 Hz）；
//!   * 推理：`ort`（onnxruntime 动态加载 `tts/onnxruntime.dll`）。
//!
//! 全部离线，无需联网；单句 30 字合成约 0.2~0.9 s（RTF≈0.14），语速≈5.5 字/秒。

use ort::session::Session;
use std::collections::HashMap;
use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::os::windows::ffi::OsStrExt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Instant;

#[link(name = "kernel32")]
unsafe extern "system" {
    fn LoadLibraryW(name: *const u16) -> *mut c_void;
    fn GetProcAddress(h: *mut c_void, name: *const c_char) -> *mut c_void;
}

fn wide(s: &str) -> Vec<u16> {
    std::ffi::OsStr::new(s).encode_wide().chain(std::iter::once(0)).collect()
}

type EspeakInit = unsafe extern "C" fn(c_int, c_int, *const c_char, c_int) -> c_int;
type EspeakSetVoice = unsafe extern "C" fn(*const c_char) -> c_int;
type EspeakTextToPhonemes = unsafe extern "C" fn(*mut *mut c_char, c_int, c_int) -> *const c_char;

/// 引擎句柄 + 会话。espeak 全局状态非线程安全，所有合成经由外层 Mutex 串行。
struct Inner {
    /// espeakbridge.pyd 的模块句柄（保持加载，勿卸载）
    _espeak_lib: usize,
    text_to_phonemes: EspeakTextToPhonemes,
    /// onnxruntime 环境必须与 Session 同寿命
    _env: ort::environment::Environment,
    session: Session,
    /// 音素（字符）→ 模型 id
    map: HashMap<char, i64>,
    sample_rate: u32,
    noise_scale: f32,
    length_scale: f32,
    noise_w: f32,
}

pub struct PiperTts {
    dir: PathBuf,
    inner: Mutex<Option<Inner>>,
    load_ms: AtomicU64,
}

impl PiperTts {
    pub fn new(dir: PathBuf) -> Self {
        Self { dir, inner: Mutex::new(None), load_ms: AtomicU64::new(0) }
    }

    /// 自检面板用：引擎就绪耗时（毫秒），0 = 未就绪（将回退系统语音）。
    pub fn load_ms(&self) -> u64 {
        self.load_ms.load(Ordering::Relaxed)
    }

    fn has_assets(&self) -> bool {
        self.dir.join("zh_CN-huayan-medium.onnx").is_file()
            && self.dir.join("espeakbridge.pyd").is_file()
            && self.dir.join("onnxruntime.dll").is_file()
    }

    fn init_inner(&self) -> Option<Inner> {
        let dir = &self.dir;
        if !self.has_assets() {
            return None;
        }
        unsafe {
            let h = LoadLibraryW(wide(&dir.join("espeakbridge.pyd").to_string_lossy()).as_ptr());
            if h.is_null() {
                return None;
            }
            let sym = |n: &str| -> Option<*mut c_void> {
                let c = CString::new(n).ok()?;
                let p = GetProcAddress(h, c.as_ptr());
                (!p.is_null()).then_some(p)
            };
            let init: EspeakInit = std::mem::transmute(sym("espeak_Initialize")?);
            let set_voice: EspeakSetVoice = std::mem::transmute(sym("espeak_SetVoiceByName")?);
            let phon: EspeakTextToPhonemes = std::mem::transmute(sym("espeak_TextToPhonemes")?);

            let data = CString::new(dir.to_string_lossy().as_bytes()).ok()?;
            // AUDIO_OUTPUT_SYNCHRONOUS = 2，返回采样率
            if init(2, 0, data.as_ptr(), 0) <= 0 {
                return None;
            }
            let voice = CString::new("cmn").ok()?;
            if set_voice(voice.as_ptr()) != 0 {
                return None;
            }

            let env = ort::init_from(dir.join("onnxruntime.dll")).ok()?.build().ok()?;
            let session = Session::builder(&env)
                .ok()?
                .commit_from_file(dir.join("zh_CN-huayan-medium.onnx"))
                .ok()?;

            let json: serde_json::Value = serde_json::from_str(
                &std::fs::read_to_string(dir.join("zh_CN-huayan-medium.onnx.json")).ok()?,
            )
            .ok()?;
            let mut map = HashMap::new();
            for (k, v) in json["phoneme_id_map"].as_object()? {
                if let Some(id) = v.as_array().and_then(|a| a.first()).and_then(|x| x.as_i64()) {
                    if let Some(ch) = k.chars().next() {
                        // 音素表键均为单码点字符
                        if k.chars().count() == 1 {
                            map.insert(ch, id);
                        }
                    }
                }
            }
            let inf = &json["inference"];
            // 允许用环境变量现场调音（比赛现场微调语速/稳定度，无需重编译）
            let env_f32 = |k: &str, def: f32| -> f32 {
                std::env::var(k).ok().and_then(|v| v.parse().ok()).unwrap_or(def)
            };
            Some(Inner {
                _espeak_lib: h as usize,
                text_to_phonemes: phon,
                _env: env,
                session,
                map,
                sample_rate: json["audio"]["sample_rate"].as_u64().unwrap_or(22050) as u32,
                noise_scale: env_f32(
                    "QX_TTS_NOISE_SCALE",
                    inf["noise_scale"].as_f64().unwrap_or(0.667) as f32,
                ),
                length_scale: env_f32(
                    "QX_TTS_LENGTH_SCALE",
                    // 默认 0.95：huayan 在 1.0 时整体偏慢（长答案约 3.6 字/秒），
                    // 略微压缩节奏后接近真人讲解的 4.2 字/秒，现场可再用环境变量微调。
                    (inf["length_scale"].as_f64().unwrap_or(1.0) as f32) * 0.95,
                ),
                noise_w: env_f32("QX_TTS_NOISE_W", inf["noise_w"].as_f64().unwrap_or(0.8) as f32),
            })
        }
    }

    /// 预热：应用启动线程调用，把 ORT 会话与模型加载藏在启动阶段。
    pub fn preload(&self) {
        let t0 = Instant::now();
        let mut guard = self.inner.lock().unwrap();
        if guard.is_none() {
            *guard = self.init_inner();
        }
        if let Some(inner) = guard.as_mut() {
            // 空跑一小段，确保推理路径与内存分配就绪
            let _ = run_once(inner, "好");
            self.load_ms.store(t0.elapsed().as_millis() as u64, Ordering::Relaxed);
        }
    }

/// 合成中文文本为 16-bit PCM 单声道 wav（22.05 kHz）。失败返回 None（调用方回退）。
///
/// 长句一次性喂给 VITS 容易出现韵母拖沓、句间粘连甚至杂音碎片，所以先做
/// 文本规整（缩写/符号）再按标点分句，逐句合成后用 160 ms 静音拼接；
/// 这同时让句间有自然停顿，听感更接近真人播报。
pub fn synth(&self, text: &str) -> Option<Vec<u8>> {
    let pieces = split_for_tts(text);
    if pieces.is_empty() {
        return None;
    }
    let t0 = Instant::now();
    let mut guard = self.inner.lock().unwrap();
    if guard.is_none() {
        *guard = self.init_inner();
    }
    let inner = guard.as_mut()?;

    let rate = inner.sample_rate as f32;
    let gap = vec![0.0f32; (0.10 * rate) as usize];
    let mut pcm: Vec<f32> = Vec::new();
    for piece in pieces.iter() {
        let part = run_once(inner, piece)?;
        let part = trim_silence(&part, rate);
        if part.is_empty() {
            continue;
        }
        if !pcm.is_empty() {
            pcm.extend_from_slice(&gap);
        }
        pcm.extend_from_slice(&part);
    }
    if pcm.is_empty() {
        return None;
    }
    if self.load_ms.load(Ordering::Relaxed) == 0 {
        self.load_ms.store(t0.elapsed().as_millis() as u64, Ordering::Relaxed);
    }
    Some(wav_bytes(&pcm, rate as u32))
}
}

/// TTS 专用文本规整：拉丁缩写念中文、去掉朗读无意义的符号。
fn normalize_for_tts(text: &str) -> String {
    let mut s = text.to_string();
    for (from, to) in [
        ("AI 智能讲解员", "人工智能讲解员"),
        ("AI智能讲解员", "人工智能讲解员"),
        ("AI", "人工智能"),
        ("VRM", "虚拟形象"),
        ("3D", "三维"),
        ("cm", "厘米"),
        ("km", "公里"),
        ("%", "百分之"),
    ] {
        s = s.replace(from, to);
    }
    // 「」《》等只做视觉强调，朗读时去掉即可；破折号与省略号转成停顿
    for ch in ['「', '」', '『', '』', '《', '》', '"', '"', '\'', '\'', '*', '#', '`'] {
        s = s.replace(ch, "");
    }
    s = s.replace("——", "，").replace("……", "，");
    s
}

/// 分句策略（实测结论）：VITS 每段都会带一段模型自身的前后静音，分得越碎整体
/// 节奏越慢（同样文本 137 字：整段 33 s，切 4 段 46 s）。因此**默认整段合成**，
/// 只有超过 120 字的长答案才按句号分组切开，兼顾流畅与长文本稳定性。
fn split_for_tts(text: &str) -> Vec<String> {
    let normalized = normalize_for_tts(text);
    let total = normalized.chars().count();
    if total <= 120 {
        let t = normalized.trim();
        return if t.is_empty() { Vec::new() } else { vec![t.to_string()] };
    }
    let mut out = Vec::new();
    let mut buf = String::new();
    for raw in normalized.split_inclusive(['。', '！', '？', '；', '\n', '!', '?', ';']) {
        if buf.chars().count() + raw.chars().count() > 120 && !buf.is_empty() {
            out.push(buf.clone());
            buf.clear();
        }
        buf.push_str(raw);
    }
    if !buf.trim().is_empty() {
        out.push(buf);
    }
    out
}

/// 去掉一段语音首尾的静音（各留 40 ms 余量），减少拼接处的顿挫感。
fn trim_silence(samples: &[f32], rate: f32) -> Vec<f32> {
    const TH: f32 = 0.03;
    let margin = (0.02 * rate) as usize;
    let start = samples.iter().position(|s| s.abs() > TH).unwrap_or(0).saturating_sub(margin);
    let end = samples
        .iter()
        .rposition(|s| s.abs() > TH)
        .map(|i| (i + margin).min(samples.len()))
        .unwrap_or(0);
    if end <= start {
        return Vec::new();
    }
    samples[start..end].to_vec()
}

fn phonemize(inner: &Inner, text: &str) -> Option<String> {
    let buf = CString::new(text).ok()?;
    let mut ptr = buf.as_ptr() as *mut c_char;
    let mut out = String::new();
    unsafe {
        loop {
            // espeakCHARS_UTF8 = 1, espeakPHONEMES_IPA = 2
            let p = (inner.text_to_phonemes)(&mut ptr, 1, 2);
            if p.is_null() {
                break;
            }
            let s = CStr::from_ptr(p).to_string_lossy();
            if s.is_empty() {
                break;
            }
            // 中文标点会被 espeak 切成独立 clause；clause 之间补一个空格音素，
            // 与 huayan 训练时的音素序列一致（A/B 实测：去掉空格会让专名连读，
            // 「梁启超故居」等识别率明显下降）。
            if !out.is_empty() {
                out.push(' ');
            }
            out.push_str(&s);
        }
    }
    (!out.is_empty()).then_some(out)
}

fn run_once(inner: &mut Inner, text: &str) -> Option<Vec<f32>> {
    let ipa = phonemize(inner, text)?;
    // 与 Piper 官方 phonemes_to_ids 完全对齐：[BOS, PAD, ph, PAD, ..., EOS]。
    // 旧实现漏掉 BOS 后的 PAD，使整条序列错位一格，模型对起始音素的时长/对齐
    // 判断失准，容易出现句首杂音与首字读错。
    let mut ids: Vec<i64> = vec![1, 0]; // BOS + PAD
    for ch in ipa.chars() {
        if let Some(&id) = inner.map.get(&ch) {
            ids.push(id);
            ids.push(0); // PAD
        }
    }
    ids.push(2); // EOS
    if ids.len() < 4 {
        return None;
    }
    let n = ids.len();
    let outputs = inner
        .session
        .run(ort::inputs![
            "input" => ort::value::Tensor::from_array((vec![1usize, n], ids)).ok()?,
            "input_lengths" => ort::value::Tensor::from_array((vec![1usize], vec![n as i64])).ok()?,
            "scales" => ort::value::Tensor::from_array((
                vec![3usize],
                vec![inner.noise_scale, inner.length_scale, inner.noise_w],
            ))
            .ok()?,
        ])
        .ok()?;
    let (_, data) = outputs[0].try_extract_tensor::<f32>().ok()?;
    (!data.is_empty()).then(|| data.to_vec())
}

fn wav_bytes(samples: &[f32], rate: u32) -> Vec<u8> {
    // 峰值归一到 0.92：Piper 原始输出电平偏低（RMS≈0.07），直接播放显得
    // 「不清晰、闷」，归一化后再写入，听感更饱满通透。
    let peak = samples.iter().fold(0.0f32, |m, s| m.max(s.abs()));
    let gain = if peak > 0.001 { (0.92 / peak).min(4.0) } else { 1.0 };
    let data: Vec<u8> = samples
        .iter()
        .flat_map(|s| ((((s * gain).clamp(-1.0, 1.0)) * 32767.0) as i16).to_le_bytes())
        .collect();
    let mut w = Vec::with_capacity(44 + data.len());
    w.extend(b"RIFF");
    w.extend(((36 + data.len()) as u32).to_le_bytes());
    w.extend(b"WAVEfmt ");
    w.extend(16u32.to_le_bytes());
    w.extend(1u16.to_le_bytes()); // PCM
    w.extend(1u16.to_le_bytes()); // mono
    w.extend(rate.to_le_bytes());
    w.extend((rate * 2).to_le_bytes());
    w.extend(2u16.to_le_bytes());
    w.extend(16u16.to_le_bytes());
    w.extend(b"data");
    w.extend((data.len() as u32).to_le_bytes());
    w.extend(data);
    w
}


#[cfg(test)]
mod tests {
    use super::*;

    /// 手动跑（--ignored）：导出语音探针 wav，用于听感与识别率 A/B 对比。
    #[test]
    #[ignore = "手动导出探针音频"]
    fn dump_tts_probe() {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tts");
        let tts = PiperTts::new(dir);
        tts.preload();
        assert!(tts.load_ms() > 0, "Piper 引擎应就绪");
        let probe = concat!(
            "开平碉楼、赤坎骑楼街、新会陈皮、梁启超故居、小鸟天堂、侨批银信。",
            "我是 AI 智能讲解员小侨，慢慢走，侨乡的故事都在砖缝、信纸和柑皮里。",
            "开平的碉楼是华侨把在海外挣下的钱寄回家一层一层盖起来的，楼身是中式的，",
            "可柱式、栏杆和顶上的尖顶又带着意大利和拜占庭的样子，所以被叫做开平第一楼。",
            "小时候在潭江边长大，天天看这些楼，却一直不知道它们背后的故事。"
        );
        let wav = tts.synth(probe).expect("合成失败");
        let out = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../.diag/probe.wav");
        std::fs::write(&out, &wav).unwrap();
        println!("probe -> {}", out.display());
    }
}
