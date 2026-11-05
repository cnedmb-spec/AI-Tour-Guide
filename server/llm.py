# -*- coding: utf-8 -*-
"""本地大模型接入（Ollama）。

侨乡知识库负责「讲得准、讲得快」，本地视觉大模型负责「问什么都会、看图也懂」。
同一个 Qwen3.5-4B 既接文字也接图片，不必在 8GB 显存里塞两个模型。
Qwen3.5 自带思考模式，但思考 token 会吃掉 num_predict 让正式回答变空，
这里统一 think=False，保证观众等的是答案不是内心独白。
Ollama 没起、模型没拉、或者比赛现场断网时，这一层直接判定不可用，
调用方回落到知识库，不影响演示。
"""
import json
import os
import time
import urllib.request

HOST = os.environ.get("OLLAMA_HOST", "http://127.0.0.1:11434").rstrip("/")
# 一个模型同时承担文本与识图：Q4_K_M 约 2.7GB + 视觉投影器，8GB 显存够用
MODEL = os.environ.get("LLM_MODEL", "qwen3.5:4b")
TIMEOUT = float(os.environ.get("LLM_TIMEOUT", "45"))
# 想给 3D 让出更多显存时设 12~16（层数放 CPU），不设则由 Ollama 自行铺满
NUM_GPU = int(os.environ.get("LLM_NUM_GPU", "0") or 0)

_ready = None
_ready_at = 0.0
_READY_TTL = 30.0    # 探测失败后的重试间隔（秒）：Ollama 晚启动/短暂故障后能自动恢复
_READY_TTL_OK = 600.0  # 成功结果的复核间隔（秒）：Ollama 中途宕机不至于长期误报可用
_last_error = ""


def _post(path: str, payload: dict):
    body = json.dumps(payload, ensure_ascii=False).encode("utf-8")
    req = urllib.request.Request(
        f"{HOST}{path}", data=body, headers={"Content-Type": "application/json"})
    with urllib.request.urlopen(req, timeout=TIMEOUT) as r:
        return json.loads(r.read().decode("utf-8"))


def available() -> bool:
    """探测 Ollama 与本机模型是否就绪。

    成功结果在 TTL 内直接复用，过期后重新复核；失败结果按更短的 TTL 重试。
    """
    global _ready, _ready_at, _last_error
    now = time.time()
    if _ready:
        if (now - _ready_at) < _READY_TTL_OK:
            return True
        _ready = None   # 成功状态过期：重新复核，避免 Ollama 宕机后仍报「可用」
    if _ready is False and (now - _ready_at) < _READY_TTL:
        return False
    try:
        with urllib.request.urlopen(f"{HOST}/api/tags", timeout=2.5) as r:
            data = json.loads(r.read().decode("utf-8"))
        names = [m.get("name", "") for m in data.get("models", [])]
        base = MODEL.split(":")[0]
        _ready = any(n == MODEL or n.split(":")[0] == base for n in names)
        if not _ready:
            _last_error = f"模型未安装：{MODEL}"
    except Exception as exc:
        _ready = False
        _last_error = str(exc)
    _ready_at = time.time()
    return _ready


def status() -> dict:
    return {"available": available(), "model": MODEL, "host": HOST, "error": _last_error}


def chat(messages, num_predict: int = 200, temperature: float = 0.65, images=None) -> str:
    """images 传 base64 字符串列表（不含 data: 前缀），会挂到最后一条用户消息上。"""
    options = {
        "num_predict": num_predict,
        "temperature": temperature,
        "top_p": 0.9,
        "num_ctx": 4096,
    }
    if NUM_GPU:
        options["num_gpu"] = NUM_GPU
    msgs = [dict(m) for m in messages]
    if images:
        msgs[-1]["images"] = images
    try:
        data = _post("/api/chat", {
            "model": MODEL,
            "messages": msgs,
            "stream": False,
            # 关掉思考：Qwen3.5 的思考 token 会占满 num_predict，正式回答反而变空
            "think": False,
            "options": options,
        })
    except Exception:
        # 调用失败：失效成功缓存，让 available() 在 TTL 后重新探测，
        # 自检/指标就不会在 Ollama 宕机后还长期显示「通过」（DEF-18）
        global _ready, _ready_at
        _ready = False
        _ready_at = time.time()
        raise
    return (data.get("message") or {}).get("content", "").strip()


def warmup() -> bool:
    """把模型预先读进显存/内存，避免现场第一次提问卡十几秒。"""
    if not available():
        return False
    try:
        chat([{"role": "user", "content": "你好"}], num_predict=6, temperature=0.2)
        return True
    except Exception as exc:
        global _ready, _ready_at, _last_error
        _ready = False
        _ready_at = time.time()
        _last_error = str(exc)
        return False
