# -*- coding: utf-8 -*-
"""离线语音合成：Piper + 口型时间轴生成"""
import hashlib
import threading
import wave
from array import array
from pathlib import Path

from piper import PiperVoice, SynthesisConfig
from pypinyin import lazy_pinyin, Style

BASE = Path(__file__).resolve().parent
CACHE = BASE / "cache"
CACHE.mkdir(exist_ok=True)
VOICE_PATH = BASE / "models" / "piper" / "zh_CN-huayan-medium.onnx"

_voice = None
_voice_lock = threading.Lock()   # warmup 线程与首个请求并发初始化时只加载一次（SUS-05）

# 语速略提一点，长回答不至于拖沓；改过音频处理就换缓存版本号，旧缓存自动失效
_CACHE_VER = "v2"
_SYN = SynthesisConfig(length_scale=0.92)


def _get_voice():
    global _voice
    if _voice is None:
        with _voice_lock:
            if _voice is None:
                _voice = PiperVoice.load(str(VOICE_PATH))
    return _voice


_AA = set("aāáǎà")
_OH = set("oōóǒò")
_EE = set("eēéěè")
_IH = set("iīíǐì")
_OU = set("uūúǔùüǖǘǚǜ")


def _viseme_of(syllable: str) -> str:
    """按韵腹归类到五个基本口型，用于驱动 VRM 表情通道"""
    for ch in reversed(syllable):
        if ch in _AA:
            return "aa"
        if ch in _OH:
            return "oh"
        if ch in _EE:
            return "ee"
        if ch in _IH:
            return "ih"
        if ch in _OU:
            return "ou"
    return "aa"


def _viseme_track(text: str, duration: float):
    sylls = [s for s in lazy_pinyin(text, style=Style.NORMAL, errors="ignore") if s.strip()]
    if not sylls:
        return []
    nsyl = len(sylls)
    # 按音节长度微调时长，长音节张得更久，短音节（如"的""了"）压缩
    weights = [max(1.0, len(s) * 0.9) for s in sylls]
    total = sum(weights)
    track = []
    t = 0.0
    for s, w in zip(sylls, weights):
        d = duration * w / total
        track.append({"t": round(t, 3), "d": round(d, 3), "v": _viseme_of(s)})
        t += d
    return track


def _tighten_silence(samples, rate, keep_ms=120, min_gap_ms=110, ratio=0.05):
    """Piper 是逐句合成后直接拼起来的，句号处的停顿能到 0.5 秒；
    把超过 110ms 的静音压到 120ms 以内，长回答听起来才连贯。"""
    win = max(1, int(rate * 0.01))          # 10ms 一窗
    n = len(samples)
    rms = []
    for i in range(0, n - win + 1, win):
        seg = samples[i:i + win]
        rms.append((sum(v * v for v in seg) / win) ** 0.5)
    if not rms:
        return samples
    thr = (max(rms) or 1) * ratio
    keep = int(rate * keep_ms / 1000)
    min_win = max(1, int(min_gap_ms / 10))
    out = array("h")
    pos = 0
    i, m = 0, len(rms)
    while i < m:
        if rms[i] < thr:
            j = i
            while j < m and rms[j] < thr:
                j += 1
            if j - i >= min_win:
                a = i * win
                out.extend(samples[pos:a + keep])
                pos = j * win
            i = j
        else:
            i += 1
    out.extend(samples[pos:])
    return out


def _rewrite_audio(src, dst):
    with wave.open(str(src), "rb") as w:
        rate, ch, sw = w.getframerate(), w.getnchannels(), w.getsampwidth()
        raw = w.readframes(w.getnframes())
    if sw != 2 or ch != 1:                  # 只处理 16bit 单声道，其余原样保留
        with wave.open(str(dst), "wb") as w:
            w.setnchannels(ch)
            w.setsampwidth(sw)
            w.setframerate(rate)
            w.writeframes(raw)
        return
    samples = _tighten_silence(array("h", raw), rate)
    with wave.open(str(dst), "wb") as w:
        w.setnchannels(ch)
        w.setsampwidth(sw)
        w.setframerate(rate)
        w.writeframes(samples.tobytes())


_synth_lock = threading.Lock()      # Piper 模型单例非线程安全，合成调用串行化
_key_locks: dict = {}
_key_locks_guard = threading.Lock()


def _key_lock(key: str) -> threading.Lock:
    with _key_locks_guard:
        lock = _key_locks.get(key)
        if lock is None:
            lock = threading.Lock()
            _key_locks[key] = lock
        return lock


def cleanup_cache(max_age_days: float = 7, max_files: int = 600) -> int:
    """启动时清理长期累积的语音缓存：先删过期文件，再按数量上限淘汰最旧的。

    长期运行下 cache 会无限增长（实测 88 MB+）；删除是安全的——
    synthesize 发现文件不存在会按 key 加锁重新合成，已读进内存的音频不受影响。
    """
    import time
    now = time.time()
    removed = 0
    try:
        for p in CACHE.glob("*.wav"):
            try:
                if now - p.stat().st_mtime > max_age_days * 86400:
                    p.unlink()
                    removed += 1
            except OSError:
                pass
        rest = sorted(
            (p for p in CACHE.glob("*.wav") if p.is_file()),
            key=lambda p: p.stat().st_mtime,
            reverse=True,
        )
        for p in rest[max_files:]:
            try:
                p.unlink()
                removed += 1
            except OSError:
                pass
    except OSError:
        pass
    return removed


def synthesize(text: str):
    """返回 (音频相对路径, 时长, 口型轨道)。同一文本命中缓存。

    同一句话并发请求只合成一次：按 key 加锁；缓存文件损坏时自动重建，
    不再把坏 wav 永久毒化缓存（此前并发写曾导致 "not RIFF id" 500）。
    """
    text = text.strip()
    key = hashlib.sha1((_CACHE_VER + text).encode("utf-8")).hexdigest()[:16]
    wav_path = CACHE / f"{key}.wav"

    with _key_lock(key):
        cached = False
        if wav_path.exists():
            try:
                with wave.open(str(wav_path), "rb") as w:
                    rate = w.getframerate()
                    frames = w.getnframes()
                    width = w.getsampwidth()
                    ch = w.getnchannels()
                    data = w.readframes(frames)
                # 完整性校验（SUS-02）：头部正常但数据被截断的 wav 判为无效，
                # 自动删除重建，避免前端 decodeAudioData 失败「有字幕没声音」
                cached = rate > 0 and len(data) == frames * width * ch
            except Exception:
                cached = False
                try:
                    wav_path.unlink()
                except OSError:
                    pass
        if not cached:
            raw_path = CACHE / f"{key}.raw.wav"
            try:
                with _synth_lock:
                    with wave.open(str(raw_path), "wb") as w:
                        _get_voice().synthesize_wav(text, w, _SYN)
                _rewrite_audio(raw_path, wav_path)
            finally:
                try:
                    raw_path.unlink()
                except OSError:
                    pass

        with wave.open(str(wav_path), "rb") as w:
            rate = w.getframerate()
            frames = w.getnframes()
    duration = frames / float(rate) if rate else 0.0

    return str(wav_path), round(duration, 2), _viseme_track(text, duration)
