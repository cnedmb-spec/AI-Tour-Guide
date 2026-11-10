"""赛前预生成高质量讲解语音（可选增强，不运行时联网）。

用途：把主题包里所有会出现在回答中的文本，用标准普通话神经语音一次性合成为
wav，按运行时的 tts-cache 键命名。构建时随 exe 打包，现场直接命中缓存播放，
**演示全程离线**。未覆盖的自由回答仍由本地 Piper 模型离线合成。

当前音色：zh-CN-XiaoyiNeural（小伊，标准普通话·卡通活泼女声），
与 Q 版熊猫吉祥物配套；仍是赛前预生成、现场离线播放，不引入运行时联网。

用法（仅需赛前跑一次，需要联网）：
    python tools/prebuild_voices.py
    python tools/prebuild_voices.py --voice zh-CN-YunxiaNeural   # 切换音色示例
    python tools/prebuild_voices.py --force          # 换音色/改文案后强制重生成

依赖（仅构建期）：pip install edge-tts ；ffmpeg（PATH 或 imageio-ffmpeg）。
删除 prebuilt-voices/ 目录即可完全退回纯离线 Piper 方案。
"""
from __future__ import annotations

import argparse
import asyncio
import json
import os
import shutil
import struct
import subprocess
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
THEME = ROOT / "themes" / "qiaoxiang" / "theme.json"
OUT = ROOT / "prebuilt-voices"
CACHE_VERSION = "v8"  # 必须与 qx-desktop/src/http.rs 的 tts_key 一致
                       # v8：换成卡通活泼女声，合成结果变化必须升版本，
                       # 否则现场会继续命中旧音色的 v7 缓存。


def fnv1a(text: str) -> int:
    """与 Rust 端 http.rs::fnv1a 完全一致的 FNV-1a 64-bit。"""
    h = 0xCBF29CE484222325
    for b in text.encode("utf-8"):
        h ^= b
        h = (h * 0x100000001B3) & 0xFFFFFFFFFFFFFFFF
    return h


def cache_key(text: str) -> str:
    return f"{fnv1a(CACHE_VERSION + text):016x}"


def collect_texts(theme: dict) -> list[tuple[str, str]]:
    """收集所有可能作为 answer.reply 的文本（返回 (来源标签, 文本)）。"""
    out: list[tuple[str, str]] = []
    for k in ("welcome", "realtime", "route", "fallback"):
        v = theme.get(k, "")
        if isinstance(v, str) and v.strip():
            out.append((k, v))
    for ex in theme.get("exhibits", []):
        eid = ex.get("id", "?")
        for k in ("brief", "script", "deep"):
            v = ex.get(k, "")
            if isinstance(v, str) and v.strip():
                out.append((f"{eid}/{k}", v))
        for i, f in enumerate(ex.get("faq", [])):
            v = f.get("a", "")
            if isinstance(v, str) and v.strip():
                out.append((f"{eid}/faq{i}", v))
    for i, c in enumerate(theme.get("chitchat", [])):
        v = c.get("a", "")
        if isinstance(v, str) and v.strip():
            out.append((f"chitchat{i}", v))
    # 去重（保留首个来源标签）
    seen, uniq = set(), []
    for tag, text in out:
        if text not in seen:
            seen.add(text)
            uniq.append((tag, text))
    return uniq


def find_ffmpeg() -> str:
    p = shutil.which("ffmpeg")
    if p:
        return p
    try:
        import imageio_ffmpeg

        return imageio_ffmpeg.get_ffmpeg_exe()
    except Exception:
        pass
    raise RuntimeError("未找到 ffmpeg，请安装并加入 PATH，或 pip install imageio-ffmpeg")


def wav_seconds(path: Path) -> float:
    with path.open("rb") as f:
        b = f.read(4096)
    if b[:4] != b"RIFF" or b[8:12] != b"WAVE":
        return 0.0
    i = 12
    rate = 22050
    data_size = 0
    while i + 8 <= len(b):
        cid, sz = b[i : i + 4], struct.unpack("<I", b[i + 4 : i + 8])[0]
        if cid == b"fmt ":
            rate = struct.unpack("<I", b[i + 12 : i + 16])[0]
        if cid == b"data":
            data_size = sz
            break
        i += 8 + sz + (sz % 2)
    return data_size / (rate * 2) if data_size else 0.0


async def generate(texts, voice: str, rate: str, force: bool, ffmpeg: str):
    import edge_tts

    OUT.mkdir(parents=True, exist_ok=True)
    manifest = []
    # 换音色时不能只按「文件已存在」判命中，否则旧音色文件会被当成新音色留下来。
    # 清单记录每条 wav 的实际音色；无记录或音色不同一律重合成。
    prev_voice: dict[str, str | None] = {}
    mf = ROOT / "voice_manifest.json"
    if mf.is_file():
        try:
            for it in json.loads(mf.read_text(encoding="utf-8")):
                prev_voice[it["file"]] = it.get("voice")
        except Exception:
            pass
    with tempfile.TemporaryDirectory(prefix="qx-voice-") as tmp:
        tmp = Path(tmp)
        for idx, (tag, text) in enumerate(texts, 1):
            key = cache_key(text)
            target = OUT / f"{key}.wav"
            if target.is_file() and not force and prev_voice.get(target.name) == voice:
                manifest.append({"tag": tag, "text": text, "file": target.name,
                                 "seconds": round(wav_seconds(target), 2), "cached": True,
                                 "voice": voice, "rate": rate})
                print(f"[{idx:2d}/{len(texts)}] 命中已有 {target.name}")
                continue
            mp3 = tmp / f"{key}.mp3"
            last_err = None
            for attempt in range(3):
                try:
                    comm = edge_tts.Communicate(text, voice, rate=rate)
                    await comm.save(str(mp3))
                    last_err = None
                    break
                except Exception as e:  # 网络抖动重试
                    last_err = e
                    await asyncio.sleep(1.5 * (attempt + 1))
            if last_err is not None:
                print(f"[{idx:2d}/{len(texts)}] 失败 {tag}: {last_err}", file=sys.stderr)
                continue
            subprocess.run(
                [ffmpeg, "-y", "-loglevel", "error", "-i", str(mp3),
                 "-ac", "1", "-ar", "22050", "-c:a", "pcm_s16le", str(target)],
                check=True,
            )
            sec = wav_seconds(target)
            manifest.append({"tag": tag, "text": text, "file": target.name,
                             "seconds": round(sec, 2), "cached": False,
                             "voice": voice, "rate": rate})
            print(f"[{idx:2d}/{len(texts)}] {tag:22s} {len(text):3d}字 -> {sec:5.1f}s")
    mismatched = [m["file"] for m in manifest if m["voice"] != voice]
    if mismatched:
        raise RuntimeError(f"{len(mismatched)} 条语音音色与目标 {voice} 不一致")
    # 清单放在目录外，保证 prebuilt-voices/ 里只有缓存 wav，可直接打包
    (ROOT / "voice_manifest.json").write_text(
        json.dumps(manifest, ensure_ascii=False, indent=2), encoding="utf-8"
    )
    return manifest


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--voice", default="zh-CN-XiaoyiNeural",
                    help="神经语音音色，默认小伊（标准普通话·卡通活泼女声，"
                         "匹配 Q 版熊猫吉祥物；旧版晓晓偏新闻播报腔）")
    ap.add_argument("--rate", default="+10%", help="语速，默认 +10%%")
    ap.add_argument("--force", action="store_true", help="忽略已有文件，全部重生成")
    ap.add_argument("--theme", default=str(THEME))
    args = ap.parse_args()

    theme = json.loads(Path(args.theme).read_text(encoding="utf-8"))
    texts = collect_texts(theme)
    print(f"主题: {args.theme}")
    print(f"待生成 {len(texts)} 条 / 共 {sum(len(t) for _, t in texts)} 字 | 音色 {args.voice} @ {args.rate}")
    ffmpeg = find_ffmpeg()
    manifest = asyncio.run(generate(texts, args.voice, args.rate, args.force, ffmpeg))
    total = sum(m["seconds"] for m in manifest)
    size = sum((OUT / m["file"]).stat().st_size for m in manifest if (OUT / m["file"]).is_file())
    print(f"\n完成 {len(manifest)}/{len(texts)} 条，音频总长 {total:.1f}s，"
          f"占用 {size / 1e6:.1f} MB，输出目录 {OUT}")
    if len(manifest) < len(texts):
        print("部分条目生成失败，可重跑本脚本（已生成的会跳过）", file=sys.stderr)
        sys.exit(1)


if __name__ == "__main__":
    main()
