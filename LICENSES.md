# 开源组件与素材授权清单

本作品「侨乡·江门 AI 智能讲解员」由参赛团队自主开发。以下为使用的开源组件与素材来源，随作品一并提交，供知识产权核验。

## 一、开源组件

| 组件 | 版本 | 用途 | 许可证 | 项目地址 |
|---|---|---|---|---|
| Three.js | r187 | 3D 渲染 | MIT | https://github.com/mrdoob/three.js |
| @pixiv/three-vrm | 3.5.5 | VRM 数字人加载与表情 | MIT | https://github.com/pixiv/three-vrm |
| sherpa-onnx | — | 离线流式语音识别（Paraformer / SenseVoice） | Apache-2.0 | https://github.com/k2-fsa/sherpa-onnx |
| Piper | — | 离线神经网络语音合成（VITS 声学模型） | MIT | https://github.com/rhasspy/piper |
| ONNX Runtime | 1.24.1 | 神经网络推理运行时（Piper 声学模型执行） | MIT | https://github.com/microsoft/onnxruntime |
| espeak-ng | 1.52 | 中文音素化（Piper 语音前端，随 exe 分发的 `espeakbridge.pyd`） | GPL-3.0 | https://github.com/espeak-ng/espeak-ng |
| Ollama | — | 本地大模型运行时 | MIT | https://github.com/ollama/ollama |
| Qwen2.5-7B-Instruct | — | 本地大语言模型权重 | Apache-2.0 | https://huggingface.co/Qwen/Qwen2.5-7B-Instruct |
| FastAPI | — | 本地服务与 WebSocket 接口 | MIT | https://github.com/fastapi/fastapi |
| Uvicorn | — | ASGI 服务器 | BSD-3-Clause | https://github.com/encode/uvicorn |
| jieba | — | 中文分词（检索预处理） | MIT | https://github.com/fxsjy/jieba |
| edge-tts | 7.3.1 | 构建期批量生成讲解语音（仅赛前使用，未随 exe 分发） | LGPL-3.0 | https://github.com/rany2/edge-tts |
| cloudflared（仅演示用） | — | 临时公网隧道，非正式运行依赖 | Apache-2.0 | https://github.com/cloudflare/cloudflared |

许可证全文随依赖包附带（`site-packages` 内 `LICENSE` 文件），本项目未修改上述组件源码。

## 二、自建内容（团队原创）

| 内容 | 说明 |
|---|---|
| 展品三维模型 | 开平碉楼、赤坎骑楼街、梁启超故居、新会陈皮、侨批银信、小鸟天堂，均由团队成员使用 Maya 原创建模并导出 GLB，未使用第三方模型库 |
| 讲解词与问答库 | 依据公开史料（地方志、景区公开资料、公开出版物）撰写，经人工核对后写入 `themes/qiaoxiang/theme.json` |
| 场景与展台 | Three.js 程序化生成（展台、灯光、地面、粒子） |
| 界面与演示脚本 | 团队自主设计 |

## 三、数字人模型

`app/assets/models/avatar.vrm` 为可再分发的 VRM 示例模型，遵循其原始授权条款，仅用于本作品演示，不用于商业发行。

## 四、字体与音频

- 界面字体使用操作系统自带的「微软雅黑 / 思源宋体」，未打包分发字体文件。
- 讲解语音有两条来源：
  1. **赛前预生成（演示默认）**：为保证普通话标准与音质，主题包内 46 条固定讲解词由微软在线神经语音（`zh-CN-XiaoxiaoNeural`）在赛前一次性批量合成，转为本地 22050 Hz WAV 后随程序分发。**比赛现场不联网**，程序直接读取本地缓存播放。该在线服务的使用需遵守微软相应服务条款，本作品仅用于非商业教学与竞赛演示；若评委要求全链路自主可控，删除 `rust-rewrite/prebuilt-voices/` 目录重新构建即可完全回退到第 2 条。
  2. **现场离线合成（回退 / 自由问答）**：知识库未覆盖的自由回答由本地 Piper 神经网络模型实时合成（`zh_CN-huayan-medium` 女声，22050 Hz），经 espeak-ng 音素化后由 ONNX Runtime 推理；语音模型权重沿用其原始授权条款，仅用于本作品演示，不用于商业发行。
- 未使用第三方录音素材。
- 未使用第三方图片素材；页面图形均为 CSS/SVG 绘制。

## 五、数据与隐私

- 麦克风音频仅经本机回环 `127.0.0.1` 送入本地识别进程，识别完成即释放，不写入磁盘、不上传互联网。
- 对话记录仅保存在浏览器内存，刷新即清空。
- 不收集任何个人身份信息，不含第三方统计/埋点脚本。

## 六、声明

本作品所用开源组件均遵守其许可证条款；如评委需要，可现场演示依赖目录中的 LICENSE 原文。若发现任何权利瑕疵，团队将立即整改并承担相应责任。

参赛团队　2026 年 11 月
