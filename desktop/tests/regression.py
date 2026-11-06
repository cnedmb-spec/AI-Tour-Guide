# -*- coding: utf-8 -*-
"""讲解员后端回归自检（网页版 / 桌面端共用同一套接口，可反复运行）。

用法：python regression.py [http://127.0.0.1:8767]

每个用例重复 3 轮；不同用例使用不同的 X-Forwarded-For，避免撞上
/api/ask 的「每客户端 20 次/60 秒」限流（限流本身单独用固定 IP 验证）。
"""
import asyncio
import base64
import io
import json
import struct
import sys
import time
import wave

import numpy as np
import requests
import websockets

BASE = (sys.argv[1] if len(sys.argv) > 1 else "http://127.0.0.1:8767").rstrip("/")
ROUNDS = 3
RESULTS = []


def record(name, ok, detail=""):
    RESULTS.append((name, ok))
    print(("  PASS  " if ok else "  FAIL  ") + name + (("  | " + detail) if detail else ""), flush=True)


def client(round_no, tag):
    # 每个用例 + 每轮一个独立来源 IP，保证限流不污染功能测试
    return {"X-Forwarded-For": f"198.51.100.{(abs(hash(tag + str(round_no))) % 200) + 1}"}


def get(path, **kw):
    return requests.get(BASE + path, timeout=20, **kw)


def post(path, payload=None, **kw):
    return requests.post(BASE + path, json=payload or {}, timeout=90, **kw)


def ask(text, round_no, tag, **extra):
    r = post("/api/ask", {"text": text, "session": "selftest", **extra},
             headers=client(round_no, tag))
    return r.status_code, (r.json() if r.headers.get("content-type", "").startswith("application/json") else {})


def repeat(name, fn, rounds=ROUNDS):
    for i in range(rounds):
        try:
            ok, detail = fn(i)
            record(f"{name} #{i + 1}", ok, detail)
        except Exception as exc:
            record(f"{name} #{i + 1}", False, f"EXCEPTION {type(exc).__name__}: {exc}")


def main():
    print(f"=== regression against {BASE} ===\n", flush=True)

    def stats(i):
        d = get("/api/stats", headers=client(i, "stats")).json()
        ok = d.get("theme") == "qiaoxiang" and d.get("kb_exhibits") == 6 and d.get("kb_answers") == 30
        return ok, f"theme={d.get('theme')} kb={d.get('kb_exhibits')}/{d.get('kb_answers')} llm={d.get('llm', {}).get('available')}"

    def theme(i):
        d = get("/api/theme", headers=client(i, "theme")).json()
        ex = d.get("exhibits", [])
        ok = len(ex) == 6 and all(e.get("id") and e.get("prop") and len(e.get("pos", [])) == 3 for e in ex)
        return ok, f"exhibits={len(ex)} accent={d.get('palette', {}).get('accent')}"

    def guide(i):
        d = get("/api/guide", headers=client(i, "guide")).json()
        ok = bool(d.get("welcome")) and len(d.get("exhibits", [])) == 6 and d.get("kb_answers", 0) >= 30
        return ok, f"welcome={len(d.get('welcome', ''))}字 kb_answers={d.get('kb_answers')}"

    def themes(i):
        d = get("/api/themes", headers=client(i, "themes")).json()
        return any(t.get("id") == "qiaoxiang" for t in d), str(d)

    def assets(i):
        paths = ["/", "/src/main.js", "/vendor/three.module.js",
                 "/assets/models/avatar.vrm", "/assets/tex/brick_color.jpg"]
        bad = [p for p in paths if get(p, stream=True, headers=client(i, "assets")).status_code != 200]
        return not bad, "缺失: " + ",".join(bad) if bad else "5 个静态资源均 200"

    def kb(i):
        code, d = ask("介绍一下碉楼", i, "kb")
        ok = code == 200 and d.get("action") == "explain" and d.get("exhibit") == "diaolou"
        ok = ok and "碉楼" in d.get("reply", "") and len(d.get("visemes", [])) > 0 and d.get("duration", 0) > 0
        return ok, f"action={d.get('action')} exhibit={d.get('exhibit')} visemes={len(d.get('visemes', []))} dur={d.get('duration')}"

    def faq(i):
        code, d = ask("为什么叫碉楼", i, "faq")
        return code == 200 and "碉堡" in d.get("reply", ""), f"action={d.get('action')} reply={d.get('reply', '')[:28]}"

    def chitchat(i):
        code, d = ask("你好", i, "chitchat")
        return code == 200 and d.get("action") == "chat", f"action={d.get('action')}"

    def realtime(i):
        code, d = ask("今天天气怎么样", i, "realtime")
        txt = d.get("reply", "")
        return code == 200 and ("查不到" in txt or "实时信息" in txt), f"reply={txt[:30]}"

    def route(i):
        code, d = ask("参观路线怎么安排", i, "route")
        return code == 200 and d.get("action") == "route", f"action={d.get('action')}"

    def welcome(i):
        code, d = ask("", i, "welcome")
        return code == 200 and d.get("action") == "welcome" and bool(d.get("reply")), f"action={d.get('action')}"

    def deep(i):
        code, d = ask("详细讲讲碉楼", i, "deep")
        return code == 200 and len(d.get("reply", "")) > 80, f"len={len(d.get('reply', ''))}"

    def llm(i):
        code, d = ask("用一句话描写秋天", i, "llm")
        txt = d.get("reply", "")
        return code == 200 and d.get("action") == "chat" and len(txt) > 5 and "查不到" not in txt, f"reply={txt[:32]}"

    def audio(i):
        code, d = ask("你好", i, "audio")
        raw = base64.b64decode(d["audio"]) if d.get("audio") else None
        if raw is None and d.get("audio_name"):
            r = get("/api/audio/" + d["audio_name"], headers=client(i, "audio-get"))
            raw = r.content if r.status_code == 200 else None
        ok = raw is not None and raw[:4] == b"RIFF" and raw[8:12] == b"WAVE"
        with wave.open(io.BytesIO(raw or b""), "rb") as w:
            frames = w.getnframes()
        return ok and frames > 1000, f"bytes={len(raw or b'')} frames={frames}"

    def security(i):
        h = client(i, "security")
        r1 = get("/api/audio/..%2f..%2fmain.py", headers=h)
        r2 = get("/node_modules", headers=h)
        r3 = get("/package.json", headers=h)
        r4 = post("/api/theme/qiaoxiang", {}, headers={**h, "Origin": "https://evil.example"})
        # 带合法同源 Origin，才能走到「主题白名单」检查（否则先被跨站防护 403）
        r5 = post("/api/theme/not-a-theme", {}, headers={**h, "Origin": BASE})
        ok = r1.status_code == 404 and r2.status_code == 404 and r3.status_code == 404
        ok = ok and r4.status_code == 403 and r5.status_code == 404
        return ok, f"audio={r1.status_code} node_modules={r2.status_code} package={r3.status_code} cross={r4.status_code} unknown={r5.status_code}"

    def vision(i):
        png = base64.b64encode(base64.b64decode(
            "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg=="
        )).decode()
        code, d = ask("这是什么", i, "vision", images=[png])
        return code == 200 and bool(d.get("reply")), f"action={d.get('action')} reply={d.get('reply', '')[:28]}"

    print("-- 数据/静态资源 --")
    repeat("stats", stats)
    repeat("theme", theme)
    repeat("guide", guide)
    repeat("themes", themes)
    repeat("static assets", assets)

    print("\n-- 问答路由（每类 3 轮）--")
    repeat("知识库讲解", kb)
    repeat("FAQ 命中", faq)
    repeat("闲聊", chitchat)
    repeat("实时信息拦截", realtime)
    repeat("参观路线", route)
    repeat("空输入欢迎语", welcome)
    repeat("深度讲解", deep)
    repeat("开放问答(LLM)", llm, 2)
    repeat("带图问答(视觉)", vision, 2)

    print("\n-- 音频与安全 --")
    repeat("TTS 音频可播放", audio)
    repeat("接口安全边界", security)

    # 端到端 ASR：把 TTS 产出的 wav 重采样成 16k float32 送进 /ws/asr
    print("\n-- 语音识别链路 (WS) --")
    for i in range(2):
        try:
            ok, detail = asyncio.run(asr_round(i))
            record(f"WS /ws/asr #{i + 1}", ok, detail)
        except Exception as exc:
            record(f"WS /ws/asr #{i + 1}", False, f"EXCEPTION {type(exc).__name__}: {exc}")

    # 限流：固定来源连发，必须出现 429
    print("\n-- 限流 --")
    hits = {"ok": 0, "limited": 0, "other": 0}
    fixed = {"X-Forwarded-For": "203.0.113.77"}
    for _ in range(24):
        r = post("/api/ask", {"text": "", "session": "limit-test"}, headers=fixed)
        if r.status_code == 200:
            hits["ok"] += 1
        elif r.status_code == 429:
            hits["limited"] += 1
        else:
            hits["other"] += 1
    record("20次/60s 限流生效", hits["limited"] > 0, json.dumps(hits))

    passed = sum(1 for _, ok in RESULTS if ok)
    print(f"\n=== 汇总：{passed}/{len(RESULTS)} 通过 ===")
    for name, ok in RESULTS:
        if not ok:
            print("  FAILED:", name)
    return 0 if passed == len(RESULTS) else 1


async def asr_round(i):
    # 取一段真实 TTS 音频做识别输入
    r = requests.post(BASE + "/api/ask",
                      json={"text": "您好，欢迎来到侨乡江门", "session": "asr-selftest"},
                      headers={"X-Forwarded-For": f"192.0.2.{i + 10}"}, timeout=90)
    d = r.json()
    raw = base64.b64decode(d["audio"]) if d.get("audio") else None
    if raw is None and d.get("audio_name"):
        raw = requests.get(BASE + "/api/audio/" + d["audio_name"], timeout=20).content
    with wave.open(io.BytesIO(raw), "rb") as w:
        rate = w.getframerate()
        pcm = np.frombuffer(w.readframes(w.getnframes()), dtype="<i2").astype(np.float32) / 32768.0
    # 线性重采样到 16 kHz（服务端约定）
    target_len = int(len(pcm) * 16000 / rate)
    pcm16 = np.interp(np.linspace(0, len(pcm) - 1, target_len), np.arange(len(pcm)), pcm).astype("<f4")
    uri = BASE.replace("http://", "ws://").replace("https://", "wss://") + "/ws/asr"
    async with websockets.connect(uri, origin=BASE, open_timeout=8) as ws:
        # 分 8 帧喂入，模拟真实麦克风流
        for chunk in np.array_split(pcm16, 8):
            await ws.send(chunk.tobytes())
        await ws.send("END")
        msgs = []
        while True:
            m = json.loads(await asyncio.wait_for(ws.recv(), timeout=30))
            msgs.append(m)
            if m.get("type") in ("final", "error"):
                break
    final = next((m for m in msgs if m.get("type") == "final"), {})
    text = final.get("text", "")
    return bool(text) and final.get("type") == "final", f"识别结果={text[:40]!r} 帧数={len(msgs)}"


if __name__ == "__main__":
    sys.exit(main())
