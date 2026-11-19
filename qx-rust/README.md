# 侨乡 AI 智能讲解员（qx-rust）

Rust 桌面端工作空间：gpui-kit 原生窗口 + WebView 渲染 Three.js 展厅，默认以 `mascot.glb` 吉祥物外观复用 VRM 骨架。离线语音由 **cpal + valle-asr / Qwen3-ASR 0.6B** 和 **Piper VITS + ONNX Runtime** 实现，本地知识库可独立完成展项导览，Ollama 提供自由问答与看图能力。

## 工作空间结构

```text
qx-rust/
├── crates/
│   ├── qx-core/       # 主题包、知识库与 FAQ/互动题检索、Ollama 客户端
│   ├── qx-desktop/    # 主程序：桌面窗口、HTTP/WebSocket、ASR、TTS、语音烘焙
│   └── embed-model/   # 可选 GGUF + mmproj 的 exe 尾部打包工具
├── web/               # Three.js 场景、VRM/GLB 讲解员与交互 UI，随包嵌入
├── themes/            # 讲解词、FAQ、互动题库，随包嵌入
├── asr/               # Qwen3-ASR 0.6B（本地权重，不提交 Git）
├── tts/               # Piper ONNX、espeak-ng 与 ONNX Runtime 资产，随包嵌入
└── prebuilt-voices/   # 本地 Piper 预生成 WAV，随包嵌入
```

`build.rs` 把 `web/`、`themes/`、`tts/`、`prebuilt-voices/` 打进 exe，运行时释放到版本化临时目录。默认不嵌入 ASR 权重，因此**能独立启动的 exe 不等于包含语音识别模型**。

## 当前语音链路

| 环节 | 实现 | 说明 |
|------|------|------|
| 麦克风 | `cpal 0.18` | 后端直接打开默认输入设备，不再依赖浏览器音频上传 |
| ASR | `valle-asr 0.2.0` / Qwen3-ASR 0.6B | 16 kHz 单声道、能量 VAD、Partial/Final 结果；纯 Rust 本地推理 |
| TTS | Piper `zh_CN-huayan-medium.onnx` | Rust 直接驱动 ONNX Runtime，espeak-ng 负责中文音素化 |
| 运行时依赖 | `tts/onnxruntime.dll`、`tts/espeak-ng.dll` | 随内嵌资产释放并动态加载，不调用外部 `piper.exe` |
| 缓存 | `prebuilt-voices/` + `tts-cache/` | 优先播放已有 WAV，未命中时使用同一 Piper 模型实时合成 |
| 降级 | 静音 WAV + 字幕 | 本地合成失败时不切换云端音色，也不阻断文字与画面 |

Piper 调音环境变量：`QX_TTS_NOISE_SCALE`、`QX_TTS_LENGTH_SCALE`、`QX_TTS_NOISE_W`。

## ASR 模型与分发

模型查找顺序为：

1. `QX_ASR_MODEL_DIR`；
2. exe 同级 `asr/qwen3-asr-0.6b/` 或 `asr/Qwen3-ASR-0.6B/`；
3. 内嵌 bundle 的 `asr/`；
4. 开发构建的源码树模型目录。

目录需要 `config.json`、`model.safetensors`，以及 `tokenizer.json`，或旧式 `vocab.json` + `merges.txt` + `tokenizer_config.json`。模型不会在运行时下载。

默认分发方式：

```text
qx-desktop.exe
asr/qwen3-asr-0.6b/
```

只有设置 `QX_EMBED_ASR=1` 构建时才把权重随包嵌入。约 1.9 GB 的权重会使 exe 超过部分 Windows 可启动的大小范围，因此默认外置。启动时用 0.5 秒静音预热 ASR；`QX_ASR_NO_WARMUP=1` 可跳过，但首次识别会变慢。

## 构建与运行

qx-desktop 目前在 **Windows 10/11 x64** 与 **Linux x64** 上构建运行。语音链路（Piper TTS + espeak-ng + ONNX Runtime、cpal 麦克风、valle-asr）已做跨平台适配，Windows 用 `.dll`、Linux 用 `.so`，无需修改代码即可在两平台编译。

### 依赖

- stable Rust（edition 2024）；
- C/C++ 编译工具链与 CMake（构建 build.rs 与可选的 local-llm 需要）；
- **Windows**：MSVC C++ Build Tools、WebView2 Runtime（通常系统已自带）；
- **Linux**：WebKitGTK 4.0 + GTK3 开发头文件（gpui-webview / wry 所需）、ALSA（cpal 麦克风）、`libdl`（动态库加载，系统自带）；
- 默认 ASR/TTS 路径不需要 Python、CMake 或外部 `piper.exe`；
- 可选 Ollama `qwen3.5:4b`，默认地址 `http://127.0.0.1:11434`。

### 命令

```bash
# 开发运行
cargo run -p qx-desktop

# 无头 HTTP 服务
QX_HEADLESS=1 cargo run -p qx-desktop

# 全部测试
cargo test --workspace

# 发布构建
cargo build --release -p qx-desktop

# 用本地 Piper 补齐预生成语音并写报告
cargo run -p qx-desktop -- --bake-voices

# 可选 GGUF 内嵌推理
cargo run -p qx-desktop --features local-llm -- --model /path/to/model.gguf
```

LLM 参数支持 `--ollama-url`、`--ollama-model`、`--llm-backend <auto|ollama|llama|local|none>`。Ollama 不可用时，展项知识库仍可离线作答；自由问答和看图会进入明确的降级路径。

## 本机性能实测（2026-11-19）

测试模型为 `qwen3.5:4b`，使用 50 张内容不同、统一为 768×768 的图片，在模型预热后逐张发送同一问题「请用一句话描述这张图片。」。计时从客户端发送 `POST /api/ask` 开始，到收到完整回答结束；50 次均成功。

| 指标 | 耗时 |
|------|-----:|
| 平均端到端耗时 | **2029.9 ms** |
| 服务端平均处理耗时 | 2026.8 ms |
| 中位数 P50 | 2023.0 ms |
| P95 | 2419.7 ms |
| 最小值 | 1557.7 ms |
| 最大值 | 2615.6 ms |

以上是本机、当前模型与测试图片条件下的结果，不作为其他硬件的性能保证。导览册中的「识图50图均值」对应本次预热后实测；单次冷启动不包含在 50 次均值内。

## 平台注意事项

- **Windows**：当前完整测试与交付平台，使用 WebView2、cpal/WASAPI 和随包的 Piper DLL。
- **Linux**：桌面壳使用 WebKitGTK/X11，cpal 对接 ALSA，Piper TTS 加载 `.so` 动态库；语音链路已随 `piper.rs` 适配，可在 Linux 上构建运行。
- **macOS**：cpal 可对接 CoreAudio，但完整桌面与 TTS 链路尚未验证，不应按已支持平台分发。

## 日志

按天滚动、保留 14 天；Windows 默认 `%LOCALAPPDATA%\QiaoxiangGuide\logs`，可用 `QX_LOG_DIR` 覆盖，`QX_LOG` 控制过滤级别。文件落盘由 [shen_eternity_log](https://github.com/EternityShen/shen_eternity_log) 的后台 tokio 写入线程承担。详见 [LOGGING.md](LOGGING.md)。
