# -*- coding: utf-8 -*-
"""离线语音识别。

两级：
- 流式 Paraformer：边说边出字，负责「识别中」的实时预览；
- SenseVoice：松手后对整段音频重跑一遍，中文准确率高得多，作为最终结果。

两台模型都在本机跑，不联网。实测 SenseVoice 在 i7-13650HX 上处理 2 秒语音
约 0.4 秒，比流式模型还快。
"""
import threading
from pathlib import Path

import numpy as np
import sherpa_onnx

BASE = Path(__file__).resolve().parent
MODEL = BASE / "models" / "sherpa"
SENSE = BASE / "models" / "sensevoice"

_lock = threading.Lock()
_init_lock = threading.Lock()   # 模型懒加载双重检查锁：warmup 与首个请求并发时只加载一次
_recognizer = None
_offline = None

MAX_SECONDS = 30          # 单次录音上限，防止一直按着攒爆内存
MAX_SAMPLES = 16000 * MAX_SECONDS


def recognizer():
    """流式模型：只用来做实时预览。"""
    global _recognizer
    if _recognizer is None:
        with _init_lock:
            if _recognizer is None:
                _recognizer = sherpa_onnx.OnlineRecognizer.from_paraformer(
                    encoder=str(MODEL / "encoder.int8.onnx"),
                    decoder=str(MODEL / "decoder.int8.onnx"),
                    tokens=str(MODEL / "tokens.txt"),
                    num_threads=4,
                    sample_rate=16000,
                    feature_dim=80,
                    decoding_method="greedy_search",
                    enable_endpoint_detection=False,
                )
    return _recognizer


def offline_recognizer():
    """SenseVoice 离线模型：负责最终结果。"""
    global _offline
    if _offline is None:
        with _init_lock:
            if _offline is None:
                _offline = sherpa_onnx.OfflineRecognizer.from_sense_voice(
                    model=str(SENSE / "model.int8.onnx"),
                    tokens=str(SENSE / "tokens.txt"),
                    num_threads=6,
                    use_itn=True,
                    language="zh",
                )
    return _offline


def warmup():
    """预加载并跑一遍空音频，避免第一条语音卡在模型初始化上。"""
    recognizer()
    offline_recognizer()
    s = offline_recognizer().create_stream()
    s.accept_waveform(16000, np.zeros(1600, dtype=np.float32))
    offline_recognizer().decode_stream(s)


class Session:
    """一路麦克风对应一个流式会话"""

    def __init__(self):
        self.rec = recognizer()
        self.stream = self.rec.create_stream()
        # sherpa-onnx 2.x 把 decode 改名成 decode_stream，两种都兼容
        self._decode = getattr(self.rec, "decode_stream", None) or self.rec.decode
        self.buf = []
        self.n = 0

    def feed(self, pcm: bytes):
        """喂一段音频，返回实时预览文本（可能为空）。"""
        samples = np.frombuffer(pcm, dtype=np.float32)
        if self.n >= MAX_SAMPLES:
            # 达到 30 秒上限：抛错让 WS 层下发 error 帧并断开，
            # 而不是静默丢音频、前端永远停在「聆听中」（DEF-19）
            raise RuntimeError("录音已达 30 秒上限，请松手重试")
        self.buf.append(samples.copy())
        self.n += len(samples)
        with _lock:
            self.stream.accept_waveform(16000, samples)
            while self.rec.is_ready(self.stream):
                self._decode(self.stream)
            return self.rec.get_result(self.stream).strip()

    def finish(self):
        """松手后：整段音频用 SenseVoice 重跑，拿最终结果。"""
        if self.n < 1600:
            return ""
        audio = np.concatenate(self.buf)
        try:
            rec = offline_recognizer()
            s = rec.create_stream()
            s.accept_waveform(16000, audio)
            rec.decode_stream(s)
            text = (s.result.text or "").strip()
            if text:
                return text
        except Exception:
            pass
        # 兜底：离线模型失败就用流式结果
        with _lock:
            return self.rec.get_result(self.stream).strip()
