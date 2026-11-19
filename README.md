# AI-Tour-Guide · 侨乡·江门 AI 智能讲解员

面向侨乡江门展厅的 AI 导览桌面应用：gpui-kit 原生窗口 + WebView 渲染 Three.js 场景，默认使用「小侨」吉祥物 GLB 外观并复用 VRM 骨架完成口型、视线和手势。语音识别、语音合成与展项知识库均可离线运行，本地 Ollama 提供自由问答和图片理解。

前端、主题包、Piper 语音资产与预生成语音在编译期打包进 exe，运行时释放到临时目录，不依赖 Python，也不调用外部 `piper.exe`。**默认构建不把约 1.9 GB 的 ASR 权重嵌入 exe；需要语音识别时，应按下文同时分发 `asr/qwen3-asr-0.6b/`。**

---

## 一、当前功能与实现

| 功能 | 当前实现 |
|------|----------|
| **桌面端** | gpui-kit + WebView；Windows 使用 WebView2，Linux 使用 WebKitGTK |
| **三维展厅** | Three.js；开平碉楼、赤坎古镇、梁启超故居、新会陈皮、侨批银信、小鸟天堂等 GLB 展项 |
| **讲解员** | 默认 `mascot.glb` 吉祥物外观，复用 `avatar.vrm` 骨架；支持表情、口型、手势与镜头联动 |
| **离线语音识别** | `cpal` 直接采集系统麦克风，`valle-asr` 加载 Qwen3-ASR 0.6B；16 kHz 单声道、能量 VAD 分段，纯 Rust 本地推理 |
| **离线语音合成** | Rust 直接驱动 Piper VITS `zh_CN-huayan-medium.onnx`；espeak-ng 音素化，ONNX Runtime 推理 |
| **预生成语音** | `prebuilt-voices/` 中的本地 WAV 随包分发；未命中缓存时使用同一 Piper 离线模型合成 |
| **本地大模型** | 默认 Ollama `http://127.0.0.1:11434`、模型 `qwen3.5:4b`；也可选择 llama-server、可选 GGUF 内嵌推理或禁用 LLM |
| **图片理解** | 支持带图 `/api/ask`；Ollama 视觉模型优先按 `capabilities` 中的 `vision` 识别，旧版本回退模型名匹配 |
| **知识库回退** | 展项讲解、FAQ、互动题面和追问可在本地知识库直接作答；没有可用 LLM 时仍可完成馆内导览 |
| **实时信息** | 明确的天气、新闻、比分意图会尝试联网查询；失败时使用本地兜底话术，不阻断导览 |
| **零 Python 运行时** | HTTP 服务、知识库路由、ASR、TTS 与桌面窗口均由 Rust 进程承担 |

### 本机识图性能实测（2026-11-19）

使用本地 Ollama `qwen3.5:4b`，50 张内容不同、统一为 768×768 的图片，模型预热后逐张测试，问题统一为「请用一句话描述这张图片。」。计时从发送 `POST /api/ask` 到收到完整回答，50 次全部成功。

| 平均耗时 | P50 | P95 | 最快 | 最慢 |
|---------:|----:|----:|-----:|-----:|
| **2029.9 ms** | 2023.0 ms | 2419.7 ms | 1557.7 ms | 2615.6 ms |

该结果只代表本机与上述测试条件；导览册的「识图50图均值」展示同一组实测数据。

---

## 二、项目结构

```text
AI-Tour-Guide/
├── README.md                 # 项目说明（本文件）
├── LICENSES.md               # 开源组件与素材授权清单
├── LICENSE                   # 项目 MIT 许可证
└── qx-rust/                  # Rust 工作空间
    ├── Cargo.toml            # qx-core / qx-desktop / embed-model
    ├── crates/
    │   ├── qx-core/          # 主题包、知识库路由、Ollama 客户端
    │   ├── qx-desktop/       # 桌面窗口、HTTP/WebSocket、ASR、TTS、语音烘焙
    │   └── embed-model/      # 可选 GGUF + mmproj 的 exe 尾部打包工具
    ├── web/                  # Three.js + VRM/GLB 讲解员与交互界面，随包嵌入
    ├── themes/               # 讲解词、FAQ、互动题库，随包嵌入
    ├── asr/                  # Qwen3-ASR 0.6B，本地模型不提交 Git
    ├── tts/                  # Piper ONNX、espeak-ng 与 ONNX Runtime 资产，随包嵌入
    ├── prebuilt-voices/      # 预生成的本地 WAV 语音
    ├── bevy-assets/models/   # 原创展项 GLB 模型
    └── LOGGING.md            # 日志系统说明
```

---

## 三、运行架构

```text
┌──────────────────── qx-desktop（Rust）────────────────────┐
│ gpui-kit 原生窗口                                         │
│   └─ WebView：Three.js 展厅、吉祥物/VRM 骨架、聊天与控制 UI │
│ 本地 HTTP / WebSocket                                     │
│   ├─ 主题包、展项知识库、FAQ 与互动题面检索                │
│   ├─ Ollama / llama-server / 可选本地 GGUF                 │
│   ├─ 带图问答与视觉模型探测                                │
│   ├─ cpal 麦克风采集 → valle-asr / Qwen3-ASR               │
│   └─ 预生成 WAV 缓存 → Piper VITS / ONNX Runtime           │
└───────────────────────────────────────────────────────────┘
```

`web/`、`themes/`、`tts/` 和 `prebuilt-voices/` 由 `build.rs` 打进 exe。启动时释放到系统临时目录中的版本化 bundle；ASR 默认外置，避免 Windows 无法启动超过约 2 GB 的 exe。

---

## 四、语音链路

| 环节 | 实现 | 说明 |
|------|------|------|
| 麦克风采集 | `cpal` | 后端直接打开默认输入设备，支持采样格式/多声道转换与 16 kHz 重采样 |
| 语音识别 | `valle-asr 0.2.0` + Qwen3-ASR 0.6B | 本地能量 VAD 分段，Partial/Final 结果通过 `/ws/asr` 发给前端 |
| 语音合成 | Piper VITS + espeak-ng + ONNX Runtime | `zh_CN-huayan-medium`，22.05 kHz 单声道 WAV；DLL 随内嵌 TTS 资产释放并动态加载 |
| 缓存与烘焙 | `prebuilt-voices/` / `tts-cache/` | 优先播放已有 WAV；`--bake-voices` 使用本地 Piper 批量补齐缺失语音 |
| 降级 | 本地静音 WAV + 字幕 | Piper 资产不可用或合成失败时保留文字、口型和镜头流程，不调用云端 TTS |

ASR 模型查找顺序：

1. `QX_ASR_MODEL_DIR` 指定的目录；
2. exe 同级 `asr/qwen3-asr-0.6b/` 或 `asr/Qwen3-ASR-0.6B/`；
3. 内嵌/释放的 bundle `asr/`（仅 `QX_EMBED_ASR=1` 构建）；
4. 开发构建的源码树模型目录。

模型目录至少包含 `config.json`、`model.safetensors`，以及 `tokenizer.json`，或旧式 `vocab.json` + `merges.txt` + `tokenizer_config.json`。程序不会在运行时自动下载模型。

---

## 五、快速开始

### 1. 环境

当前完整交付以 **Windows 10/11 x64** 与 **Linux x64** 为主：

- 最新 stable Rust（工作空间使用 edition 2024）；
- **Windows**：MSVC C++ Build Tools、WebView2 Runtime（通常系统已自带）；
- **Linux**：WebKitGTK 4.0 + GTK3 开发头文件、ALSA（cpal 麦克风）；
- 可选：Ollama + `qwen3.5:4b`，用于自由问答和看图。

默认 ASR/TTS 路径不需要 Python，也不需要安装或单独运行 `piper.exe`。可选 `local-llm` 内嵌推理构建另需完整的 C/C++ 编译环境。

### 2. 准备 ASR 模型

开发运行时把模型放到：

```text
qx-rust/asr/qwen3-asr-0.6b/
```

分发时默认放到：

```text
qx-desktop.exe
asr/qwen3-asr-0.6b/
```

也可以用 `QX_ASR_MODEL_DIR` 指向模型目录。缺少模型时，文字问答与 TTS 仍可使用，自检会如实显示语音识别不可用。

### 3. 构建与运行

```bash
cd qx-rust

# 开发运行，默认 HTTP 端口 8765
cargo run -p qx-desktop

# 无头模式，仅启动本地 HTTP 服务
QX_HEADLESS=1 cargo run -p qx-desktop

# 全部测试
cargo test --workspace

# 发布构建
cargo build --release -p qx-desktop

# 可选：使用本地 Piper 烘焙预生成语音
cargo run -p qx-desktop -- --bake-voices
```

### 4. 大模型后端

默认连接本机 Ollama `http://127.0.0.1:11434`，模型为 `qwen3.5:4b`。Ollama 不可用时不影响本地知识库导览；自由问答与图片理解会使用相应降级话术。

可选 GGUF 内嵌推理：

```bash
cargo run -p qx-desktop --features local-llm -- --model /path/to/model.gguf
```

---

## 六、命令行参数

| 参数 | 说明 | 默认值 |
|------|------|--------|
| `--bake-voices` | 使用本地 Piper 批量生成缺失的讲解与问答 WAV，并写烘焙报告 | 关闭 |
| `--model <路径>` | 使用本地 GGUF；需要 `local-llm` feature | — |
| `--ollama-url <URL>` | Ollama / llama-server 兼容协议地址 | `http://127.0.0.1:11434` |
| `--ollama-model <名>` | Ollama 模型名 | `qwen3.5:4b` |
| `--llm-backend <auto\|ollama\|llama\|local\|none>` | 选择 LLM 后端 | `auto` |

---

## 七、常用环境变量

| 环境变量 | 说明 |
|----------|------|
| `QX_PORT` | HTTP 起始端口，默认 `8765`；被占用时向后尝试最多 8 个端口 |
| `QX_HEADLESS=1` | 只启动 HTTP 服务，不创建窗口 |
| `QX_ASR_MODEL_DIR` | 指定 Qwen3-ASR 模型目录 |
| `QX_ASR_NO_WARMUP=1` | 跳过启动时的 ASR 权重预热，首次识别会变慢 |
| `QX_EMBED_ASR=1` | 构建时嵌入 ASR 权重；默认不嵌入，超大 exe 在部分 Windows 上无法启动 |
| `QX_TTS_NOISE_SCALE` / `QX_TTS_LENGTH_SCALE` / `QX_TTS_NOISE_W` | 覆盖 Piper 推理调音参数 |
| `QX_VOICE_BAKE_DIR` | 指定语音烘焙输出根目录，默认写回编译时源码根 |
| `QX_VOICE_BAKE_CACHE` | 指定搬运运行时 WAV 缓存的来源目录 |
| `QX_NO_EMBEDDED_MODEL=1` | `local-llm` 构建中禁用 exe 内嵌模型 |
| `QX_LLM_SELFTEST=1` | 执行本地 GGUF 自检，不启动窗口或 HTTP 服务 |
| `QX_LOG_DIR` | 覆盖日志目录 |
| `QX_LOG` | 日志过滤级别，语法同 `RUST_LOG` |

---

## 八、平台说明

- **Windows**：当前完整测试与交付平台；WebView2 窗口、Piper/espeak-ng/ONNX Runtime DLL、cpal/WASAPI 均在此路径运行。
- **Linux**：桌面壳使用 WebKitGTK/X11，cpal 对接 ALSA，Piper TTS 加载 `.so` 动态库；语音链路已随 piper.rs 适配，可在 Linux 上构建运行。
- **macOS**：cpal 支持 CoreAudio，但当前完整桌面与 TTS 链路尚未作为可交付平台验证。

---

## 九、日志

按天滚动、保留 14 天，写入用户数据目录，可用 `QX_LOG_DIR` 覆盖：

- Windows：`%LOCALAPPDATA%\QiaoxiangGuide\logs`
- Linux：`~/.local/share/QiaoxiangGuide/logs`

文件落盘由 [`shen_eternity_log`](https://github.com/EternityShen/shen_eternity_log) 的后台 tokio 写入线程承担。详见 [`qx-rust/LOGGING.md`](qx-rust/LOGGING.md)。

---

## 十、授权

项目代码按 [MIT License](LICENSE) 分发。Three.js、three-vrm、valle-asr、cpal、Qwen3-ASR、Piper、ONNX Runtime、espeak-ng 等组件及素材授权见 [`LICENSES.md`](LICENSES.md)，各组件保持其原始许可证。

> 侨乡·江门 AI 智能讲解员 —— 让碉楼、骑楼、陈皮、侨批，在本地也能听见、看懂、说出口。
