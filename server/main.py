# -*- coding: utf-8 -*-
"""AI 讲解员 · 本地服务"""
import asyncio
import base64
import collections
import os
import re
import threading
import time
from contextlib import asynccontextmanager
from pathlib import Path
from urllib.parse import urlparse

from fastapi import FastAPI, Request, WebSocket, WebSocketDisconnect
from fastapi.responses import FileResponse, JSONResponse
from fastapi.staticfiles import StaticFiles
from pydantic import BaseModel

import asr
import llm
import nlu
import tts

BASE = Path(__file__).resolve().parent
ROOT = BASE.parent
APP = ROOT / "app"
PUBLIC_URL_FILE = ROOT / "PUBLIC-URL.txt"


def _tunnel_origin() -> str:
    """读取当前隧道域名（兼容 UTF-8 BOM），作为 WS/CSRF 校验的额外白名单项。"""
    try:
        raw = PUBLIC_URL_FILE.read_text(encoding="utf-8-sig").strip()
        if raw.startswith("http"):
            return (urlparse(raw).netloc or "").lower()
    except Exception:
        pass
    return ""


def _start_warmup():
    def run():
        try:
            removed = tts.cleanup_cache()
            if removed:
                print("tts cache cleaned:", removed)
        except Exception as exc:
            print("tts cache cleanup failed:", exc)
        t = time.time()
        asr.warmup()
        _metrics["asr_load_ms"] = int((time.time() - t) * 1000)
        t = time.time()
        tts._get_voice()
        _metrics["tts_load_ms"] = int((time.time() - t) * 1000)
        # 预热一条短句，避免首句卡顿；顺带量出真实 RTF，页面不再写死 1.00（DEF-10）
        t = time.time()
        _wp, _wd, _vv = tts.synthesize("您好。")
        _metrics["tts_rtf"] = round((time.time() - t) / _wd, 3) if _wd else 1.0

        # 预热本地大模型：把权重读进显存，现场第一次提问才不会卡十几秒
        t = time.time()
        if llm.warmup():
            _metrics["llm_load_ms"] = int((time.time() - t) * 1000)

        # 预热演示常用句的语音：现场第一次问这些题不用等合成
        # 注意要预热"讲解合成后的整句"，不是 FAQ 原文
        try:
            warm_qs = [f.get("q", "") for e in guide.theme.get("exhibits", []) for f in e.get("faq", [])[:1]]
            warm_qs += ["介绍你自己"]
            for q in warm_qs:
                if not q:
                    continue
                reply = guide.answer(q).get("reply", "")
                if reply:
                    tts.synthesize(reply)
        except Exception as exc:  # 预热失败不影响主流程
            print("warmup tts failed:", exc)

    threading.Thread(target=run, daemon=True).start()


@asynccontextmanager
async def _lifespan(_app):
    # 取代已弃用的 @app.on_event("startup")（OPT-05）
    _start_warmup()
    yield


app = FastAPI(title="AI Tour Guide", docs_url=None, redoc_url=None, lifespan=_lifespan)
guide = nlu.Guide()
_metrics = {"asr_load_ms": 0, "tts_load_ms": 0, "llm_load_ms": 0,
            "requests": 0, "llm_requests": 0, "tts_rtf": 0.0}


@app.middleware("http")
async def cache_headers(request, call_next):
    """代码/接口每次取最新；模型与贴图允许浏览器缓存，但每次都回源校验。

    no-store 会让 10 MB 的 VRM 和 9 MB 的 GLB 每次打开都重下——公网隧道上就是一两分钟。
    改成 max-age=0 + must-revalidate：换模型后 ETag 变了会重新下载，没换则 304 直接用缓存。
    """
    path = request.url.path
    # 静态目录挂的是整个 app/，不把依赖清单与 node_modules 暴露给访客
    if path.startswith("/node_modules") or path in ("/package.json", "/package-lock.json") or "/.git" in path:
        return JSONResponse({"error": "not found"}, status_code=404)
    resp = await call_next(request)
    if path.startswith("/assets/") or path.startswith("/vendor/") or path.startswith("/api/audio/"):
        resp.headers["Cache-Control"] = "public, max-age=0, must-revalidate"
    else:
        resp.headers["Cache-Control"] = "no-store, must-revalidate"
    # 基础安全响应头：防 MIME 嗅探 / 点击劫持 / 外链带引
    resp.headers["X-Content-Type-Options"] = "nosniff"
    resp.headers["X-Frame-Options"] = "DENY"
    resp.headers["Referrer-Policy"] = "no-referrer"
    return resp


class AskBody(BaseModel):
    text: str
    exhibit: str | None = None
    deep: bool = False
    images: list[str] = []
    session: str | None = None   # 前端会话标识：隔离各访客的大模型上下文


@app.get("/api/themes")
def themes():
    return list(guide.list_themes())


@app.get("/api/theme")
def theme_info():
    return guide.info()


@app.post("/api/theme/{theme_id}")
def switch_theme(theme_id: str, request: Request):
    # 跨站防护（DEF-21）：带 Origin 时必须与本站 Host 或本机隧道域名精确一致；
    # 经隧道进来的跨站简单请求（无 Origin 的场景由 XFF 识别）直接拒绝
    origin = request.headers.get("origin") or ""
    if origin:
        oh = (urlparse(origin).netloc or "").lower()
        host = (request.headers.get("host") or "").lower()
        if oh != host and oh != _tunnel_origin():
            return JSONResponse({"error": "origin not allowed"}, status_code=403)
    elif request.headers.get("x-forwarded-for"):
        return JSONResponse({"error": "origin required"}, status_code=403)
    # 主题白名单：id 必须真实存在于 themes/ 目录，堵住任意路径拼接
    known = {t["id"] for t in guide.list_themes()}
    if theme_id not in known:
        return JSONResponse({"error": "unknown theme"}, status_code=404)
    return guide.load(theme_id)


@app.get("/api/guide")
def guide_info():
    """讲解员展示用的公开内容：简介、示例问题、欢迎语，不含场景参数。"""
    t = guide.theme or {}
    return {
        "welcome": t.get("welcome", ""),
        "assistant": t.get("assistant", {}),
        "exhibits": [
            {
                "id": e.get("id", ""),
                "name": e.get("name", ""),
                "brief": e.get("brief", ""),
                "faq": [f.get("q", "") for f in e.get("faq", [])],
                "keywords": e.get("keywords", []),
            }
            for e in t.get("exhibits", [])
        ],
        "chitchat": [next(iter(c.get("k") or []), "") for c in t.get("chitchat", []) if c.get("k")][:4],
        "kb_answers": sum(len(e.get("faq", [])) + 2 for e in t.get("exhibits", [])),
    }


@app.get("/api/stats")
def stats():
    exhibits = guide.theme.get("exhibits", []) if guide.theme else []
    return {
        **_metrics,
        "kb_exhibits": len(exhibits),
        "kb_answers": sum(len(e.get("faq", [])) + 2 for e in exhibits),
        "theme": guide.theme_id,
        "llm": llm.status(),
    }


# ── 公网防护（DEF-02）：请求体上限 + 按客户端限流；根治仍需关闭隧道或加访问鉴权 ──
_TEXT_MAX = int(os.environ.get("GUIDE_TEXT_MAX", "500"))
_IMAGE_COUNT_MAX = int(os.environ.get("GUIDE_IMAGE_COUNT_MAX", "3"))
_IMAGE_CHARS_MAX = int(os.environ.get("GUIDE_IMAGE_CHARS_MAX", "2500000"))
_RATE_LIMIT = int(os.environ.get("GUIDE_RATE_LIMIT", "20"))
_RATE_WINDOW = float(os.environ.get("GUIDE_RATE_WINDOW", "60"))
_rate_lock = threading.Lock()
_rate_hits: dict = {}


def _client_key(request: Request) -> str:
    xff = request.headers.get("x-forwarded-for", "")
    if xff:
        return "pub:" + xff.split(",")[0].strip()
    return request.client.host if request.client else "unknown"


def _rate_ok(key: str) -> bool:
    now = time.time()
    with _rate_lock:
        dq = _rate_hits.setdefault(key, collections.deque())
        while dq and now - dq[0] > _RATE_WINDOW:
            dq.popleft()
        if len(dq) >= _RATE_LIMIT:
            return False
        dq.append(now)
        return True


@app.post("/api/ask")
def ask(body: AskBody, request: Request):
    t0 = time.time()
    if not _rate_ok(_client_key(request)):
        return JSONResponse({"error": "请求过于频繁，请稍后再试"}, status_code=429)
    if len(body.text) > _TEXT_MAX:
        return JSONResponse({"error": f"问题过长（上限 {_TEXT_MAX} 字）"}, status_code=413)
    if len(body.images) > _IMAGE_COUNT_MAX or any(len(im) > _IMAGE_CHARS_MAX for im in body.images):
        return JSONResponse({"error": "图片数量或体积超限"}, status_code=413)
    res = guide.answer(body.text, body.exhibit, body.deep, images=body.images,
                       session=(body.session or "")[:64])
    if res.get("action") == "chat" and llm.available():
        _metrics["llm_requests"] += 1
    # 回答兜底：知识库/大模型都拿不到内容时至少给一句可用话术，不返回空串
    if not (res.get("reply") or "").strip():
        res["reply"] = guide.theme.get("fallback") or "这个我一时答不上来，换个问题试试。"
        res["action"] = "fallback"
    # 合成失败不再拖垮整个接口：返回空音频，前端只显示字幕
    audio_name = ""
    try:
        tts_t0 = time.time()
        wav_path, duration, visemes = tts.synthesize(res["reply"])
        # 真实测量 TTS 实时率（DEF-10），指数平滑
        if duration and duration > 0:
            rtf = (time.time() - tts_t0) / duration
            prev = _metrics["tts_rtf"] or 0
            _metrics["tts_rtf"] = round(rtf, 3) if not prev else round(prev * 0.7 + rtf * 0.3, 3)
        raw = Path(wav_path).read_bytes()
        # 短音频内联 base64；长音频改走 /api/audio/{name}，避免数 MB 的 JSON（DEF-24）
        audio_name = Path(wav_path).name
        payload = base64.b64encode(raw).decode("ascii") if len(raw) <= 300_000 else ""
    except Exception as exc:
        print("tts synthesize failed:", exc)
        payload, duration, visemes = "", 0.0, []
    _metrics["requests"] += 1
    return {**res, "audio": payload, "audio_name": audio_name, "duration": duration,
            "visemes": visemes, "elapsed_ms": int((time.time() - t0) * 1000)}


@app.websocket("/ws/asr")
async def ws_asr(ws: WebSocket):
    # 跨站 WebSocket 防护（DEF-03）：Origin 必须与本站 Host 或本机隧道域名精确一致，
    # 不再放过任意第三方 *.trycloudflare.com；经隧道进来的请求不允许缺 Origin
    origin = ws.headers.get("origin")
    if origin:
        oh = (urlparse(origin).netloc or "").lower()
        host = (ws.headers.get("host") or "").lower()
        if oh != host and oh != _tunnel_origin():
            await ws.close(code=1008)
            return
    elif ws.headers.get("x-forwarded-for"):
        await ws.close(code=1008)
        return
    await ws.accept()
    session = await asyncio.to_thread(asr.Session)
    try:
        while True:
            # 空闲超时：客户端只连不发也不关时，释放这条会话占用的协程与识别线程
            try:
                msg = await asyncio.wait_for(ws.receive(), timeout=180)
            except asyncio.TimeoutError:
                break
            if msg.get("type") == "websocket.disconnect":
                break
            # 文本帧 "END"：客户端松手了，用离线模型出最终结果
            if msg.get("text") == "END":
                text = await asyncio.to_thread(session.finish)
                await ws.send_json({"type": "final", "text": text})
                break
            pcm = msg.get("bytes")
            if not pcm:
                continue
            text = await asyncio.to_thread(session.feed, pcm)
            if text:
                await ws.send_json({"type": "partial", "text": text})
    except WebSocketDisconnect:
        pass
    except Exception as exc:  # 麦克风异常不应该拖垮服务
        try:
            await ws.send_json({"type": "error", "text": str(exc)})
        except Exception:
            pass


_AUDIO_NAME = re.compile(r"^[0-9a-f]{16}\.wav$")


@app.get("/api/audio/{name}")
def audio(name: str):
    # 文件名白名单（sha1 前 16 位 hex + .wav），杜绝路径穿越
    if not _AUDIO_NAME.fullmatch(name):
        return JSONResponse({"error": "not found"}, status_code=404)
    path = BASE / "cache" / name
    if not path.exists():
        return JSONResponse({"error": "not found"}, status_code=404)
    return FileResponse(path, media_type="audio/wav", headers={"Content-Disposition": "inline"})


app.mount("/", StaticFiles(directory=str(APP), html=True), name="app")
