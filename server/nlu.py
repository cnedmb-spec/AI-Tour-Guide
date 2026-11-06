# -*- coding: utf-8 -*-
"""知识库检索与话术路由。

两层：侨乡知识库负责「讲得准、讲得快」，本地大模型（千问）负责「问什么都会」。
大模型不可用时自动回落到知识库，断网也能正常演示。
"""
import json
import re
import threading
from pathlib import Path

import jieba
from pypinyin import lazy_pinyin

import llm

THEMES = Path(__file__).resolve().parent.parent / "themes"

_STOP = set("的 了 是 在 我 你 他 它 这 那 有 和 与 就 都 也 吗 呢 啊 吧 请问 一下 什么 怎么 为什么 能 可以 想 要 个 件 些 谁".split())
_PUNCT = re.compile(r"[，。！？、；：,.!?;:\s“”‘’()（）《》【】]")
_MD = re.compile(r"[*#`>~|]+")

# 实时信息关键词：本机离线运行，看不了天气/新闻/行情，宁可说不知道也不臆测
_REALTIME = re.compile(
    "天气|气温|下雨|降雨|台风|温度几度|空气质量|雾霾|"
    "新闻|热搜|头条|最近发生|刚刚发生|"
    "股价|股票|汇率|比特币|基金|油价|房价|"
    "现在几点|今天几号|今天星期几|几月几号|现在时间|"
    "比分|赛况|航班|高铁票|路况|堵不堵"
)
DEFAULT_REALTIME = (
    "这类实时信息我查不到——我是本机离线跑的讲解员，没有联网看天气、看新闻。"
    "侨乡的事你尽管问，碉楼、骑楼、陈皮、侨批我都熟。"
)


def _tokens(text: str):
    text = _PUNCT.sub("", text)
    return [t for t in jieba.cut(text) if t.strip() and t not in _STOP]


def _overlap(a, b):
    sa, sb = set(a), set(b)
    if not sa or not sb:
        return 0.0
    return len(sa & sb) / len(sa)


# 疑问焦点：问「多高」和问「为什么」不是一类问题，拿同一条 FAQ 去答就是答非所问
_FOCUS = [
    ("why", re.compile("为什么|为啥|原因|怎么会")),
    ("howmany", re.compile("多少|多高|多长|多大|多远|多久|几年|几座|几个|几次|几层|多重|几岁")),
    ("when", re.compile("哪年|什么时候|何时|几几年|哪个朝代|哪一代")),
    ("who", re.compile("是谁|谁住|谁盖|谁建|哪个人|什么人|有哪些人")),
    ("where", re.compile("在哪|哪里|哪儿|什么地方")),
    ("how", re.compile("怎么|如何|怎样")),
]
_WHAT = re.compile("是什么|什么是|啥|什么意思")


def _focus(text: str):
    """抽出问题在问哪一类东西；抽不出就返回空集，不做限制。"""
    out = {name for name, pat in _FOCUS if pat.search(text or "")}
    if not out and _WHAT.search(text or ""):
        out.add("what")
    return out


def _py_key(text: str) -> str:
    """不带声调的拼音串，用来做同音纠错。"""
    return " ".join(lazy_pinyin(text))


_REDIRECT_HINT = re.compile("感兴趣|想不想|要不要|可以问我|还可以问|换个话题|关系不大|有点远|带你去看")
_REDIRECT_TOPIC = re.compile("侨乡|江门|碉楼|骑楼|展品|展厅|小鸟天堂|陈皮|侨批")


def _trim_redirect(text: str) -> str:
    """大模型爱在结尾把话题拽回展品；开放问答里这半句直接剪掉。"""
    m = re.search(r"[^。！？]*[。！？]\s*$", text)
    if not m:
        return text
    last = m.group(0)
    if _REDIRECT_HINT.search(last) and _REDIRECT_TOPIC.search(last):
        return text[:m.start()].strip()
    return text


def _clean_llm(text: str, limit: int = 90, drop_redirect: bool = False) -> str:
    """把大模型的输出整理成适合朗读的一小段口语。"""
    text = _MD.sub("", text or "")
    text = re.sub(r"\s*\n+\s*", " ", text).strip()
    text = re.sub(r"^(小侨|讲解员)[：:]\s*", "", text)
    if drop_redirect:
        text = _trim_redirect(text)
    if len(text) > limit:
        cut = max(text.rfind("。", 0, limit), text.rfind("！", 0, limit),
                  text.rfind("？", 0, limit), text.rfind("；", 0, limit))
        text = text[:cut + 1] if cut > limit * 0.45 else text[:limit] + "。"
    return text.strip()


class Guide:
    # 会话上下文上限：超过后淘汰最早创建的会话，避免长期运行内存缓慢增长
    MAX_SESSIONS = 64

    def __init__(self):
        self.theme = None
        self.theme_id = None
        # 会话 id -> LLM 上下文；不再全站共享一份 history（此前多标签/多访客会互相串话）
        self._sessions = {}
        self._pub_lock = threading.Lock()   # 主题发布锁：load() 整体构建完再原子替换
        self._sess_lock = threading.Lock()  # 会话表锁：_history 并发安全
        self._system = ""
        first = list(self.list_themes())[0]
        self.load(first["id"])

    def _history(self, session: str | None):
        sid = session or "_default"
        # 会话表本身会被多个线程池线程同时创建/淘汰，加锁（DEF-07）
        with self._sess_lock:
            h = self._sessions.get(sid)
            if h is None:
                h = []
                self._sessions[sid] = h
                while len(self._sessions) > self.MAX_SESSIONS:
                    oldest = next(iter(self._sessions))
                    self._sessions.pop(oldest, None)
        return h

    @staticmethod
    def _remember(h, user: str, assistant: str):
        h.append({"role": "user", "content": user})
        h.append({"role": "assistant", "content": assistant})
        del h[:-10]

    def list_themes(self):
        for d in sorted(THEMES.iterdir()):
            cfg = d / "theme.json"
            if cfg.exists():
                data = self._read(cfg)
                yield {"id": d.name, "name": data.get("name", d.name), "short": data.get("short", d.name)}

    @staticmethod
    def _read(path: Path):
        return json.loads(path.read_text(encoding="utf-8"))

    def load(self, theme_id: str):
        cfg_path = THEMES / theme_id / "theme.json"
        if not cfg_path.exists():
            raise FileNotFoundError(theme_id)
        # 先在局部把索引/系统提示/拼音表全部构建完，再一次原子发布（DEF-07）：
        # 并发提问不会读到「theme 已换、_tokens 还没建」的中间态
        theme = self._read(cfg_path)
        for ex in theme.get("exhibits", []):
            ex["_tokens"] = _tokens(" ".join([ex.get("name", "")] + ex.get("keywords", []) + ex.get("aliases", [])))
            for faq in ex.get("faq", []):
                faq["_tokens"] = _tokens(faq.get("q", "") + faq.get("a", ""))
        system = self._build_system(theme)
        py_map = self._build_py_map(theme)
        with self._pub_lock:
            self.theme = theme
            self.theme_id = theme_id
            self._system = system
            self._py_map = py_map
            with self._sess_lock:
                self._sessions = {}
        return self.info()

    def _build_py_map(self, t=None):
        """同音校正表：拼音 → 词条。用来把语音识别的同音错字拉回展馆词汇。

        识别模型是流式小模型，会把「碉楼」听成「调楼」、「侨批」听成「桥批」，
        这里按拼音对齐修正，只改同音字，不动其它内容。
        """
        terms = set()
        t = t or self.theme
        assistant_name = (t.get("assistant") or {}).get("name", "")
        if assistant_name:
            terms.add(assistant_name)
        for ex in t.get("exhibits", []):
            if ex.get("name"):
                terms.add(ex["name"])
            terms.update(a for a in ex.get("aliases", []) if len(a) >= 2)
            terms.update(k for k in ex.get("keywords", []) if len(k) >= 2)
        for c in t.get("chitchat", []):
            terms.update(k for k in c.get("k", []) if len(k) >= 2)
        out = {}
        for term in sorted(terms, key=len, reverse=True):
            out.setdefault(_py_key(term), term)
        return out

    def correct(self, text: str) -> str:
        """按拼音把识别结果里的同音错字改回展馆用词。"""
        if not text:
            return text
        s = text
        for n in (4, 3, 2):
            i = 0
            while i + n <= len(s):
                w = s[i:i + n]
                hit = self._py_map.get(_py_key(w))
                if hit and hit != w:
                    s = s[:i] + hit + s[i + n:]
                    i += len(hit)
                else:
                    i += 1
        return s

    def _build_system(self, t=None):
        """给大模型的角色设定：讲解员口吻 + 侨乡资料，不限话题。"""
        t = t or self.theme
        a = t.get("assistant") or {}
        lines = [
            f"你是「{a.get('name', '讲解员')}」，{t.get('name', '侨乡展厅')}的{a.get('title', 'AI 智能讲解员')}，"
            "正在向来到江门的观众介绍侨乡文化，面前是一座座立体展台。",
            "场合是江门的侨乡文化展示与旅游推广，不是博物馆，展台上摆的是碉楼、骑楼、故居、陈皮、侨批、古榕这些侨乡风物。",
            "说话要求：中文口语，第一人称，像面对面聊天；每次回答不超过 80 个字；",
            "不要用 markdown、星号、项目符号或括号注释；不要自称人工智能或语言模型，也不要说自己是「3D 数字人」。",
            "不要声称自己属于政府机关、景区或某家公司；被问到身份，就说自己是介绍江门侨乡文化的 AI 智能讲解员。",
            "任何领域的问题都可以回答——历史、地理、科学、文学、写句子、闲聊都行，回答要自然，不要生硬地拉回展品。",
            "答完就停：末尾不要反问「这和侨乡有什么关系」，也不要劝对方改听别的展品。",
            "你不在联网状态，拿不到实时信息（天气、新闻、比分、股价、日程等），被问到时老实说查不到，别编这类数字。",
            "但历史、地理、科学、文学这些常识问题直接按你掌握的知识回答；具体数字记不准就给个大概范围，不要笼统地说「查不到」。",
            "只有在对方问起展品、侨乡文化或参观路线时，才使用下面这份资料，不要编造数据。",
            "",
            "【你熟悉的展品】",
        ]
        for ex in t.get("exhibits", []):
            lines.append(f"- {ex.get('name', '')}：{ex.get('brief', '')}")
        lines += [
            "",
            "【地方背景】江门俗称五邑，指新会、台山、开平、恩平、鹤山，海外乡亲四百多万，"
            "侨批、碉楼、骑楼、陈皮都从这里来。",
            f"【参观路线】{t.get('route', '')}",
        ]
        return "\n".join(lines)

    def info(self):
        t = self.theme or {}
        name = t.get("name", self.theme_id or "")
        info = {
            "id": self.theme_id,
            "name": name,
            "short": t.get("short", name),
            "assistant": t.get("assistant", {}),
            "palette": t.get("palette", {}),
            "scene": t.get("scene", "hall"),
            "exhibits": [
                {
                    "id": e.get("id", ""),
                    "name": e.get("name", ""),
                    "prop": e.get("prop"),
                    "pos": e.get("pos", [0, 0, -2]),
                    **({"model": e["model"]} if e.get("model") else {}),
                    **({"modelScale": e["modelScale"]} if e.get("modelScale") else {}),
                    **({"materials": e["materials"]} if e.get("materials") else {}),
                    **({"plaque": e["plaque"]} if e.get("plaque") else {}),
                    **({"shift": e["shift"]} if e.get("shift") else {}),
                }
                for e in t.get("exhibits", [])
            ],
        }
        # 主题包可选字段：开场白与「展品 → 情绪/手势」风格表。
        # 只在存在时下发；缺省时前端仍用内置默认值，换展不必改代码。
        if t.get("welcome"):
            info["welcome"] = t["welcome"]
        if t.get("exhibit_styles"):
            info["exhibit_styles"] = t["exhibit_styles"]
        return info

    def exhibit(self, exc_id):
        for e in self.theme.get("exhibits", []):
            if e.get("id") == exc_id:
                return e
        return None

    def answer(self, text: str, exhibit_id=None, deep=False, images=None, session=None):
        text = self.correct((text or "").strip())
        raw = _PUNCT.sub("", text)

        # 0. 带图问答：先走视觉模型；知识库只作为事实补充
        if images:
            reply = self.vision_answer(text, images, exhibit_id, session)
            if reply:
                ex = self.exhibit(exhibit_id) if exhibit_id else None
                return self._pack(reply, "explain" if ex else "chat", ex)
            if not text:
                return self._pack(self.theme.get("fallback", ""), "fallback")
            # 视觉模型不可用/识别失败：退回普通文本问答路径，
            # 别把用户随图输入的文字问题一起丢掉（DEF-20）

        tokens = _tokens(text)

        if not raw:
            return self._pack(self.theme.get("welcome", ""), "welcome", emotion="happy")

        # 0. 实时信息（天气/新闻/股价/时间…）：本机离线跑，一律不臆测，不交给大模型
        if _REALTIME.search(raw):
            return self._pack(self.theme.get("realtime", DEFAULT_REALTIME), "fallback", emotion="neutral")

        # 1. 闲聊 / 常见问答
        # 只认「整句就是在聊这个」：像「江门」这种短词不能把「写一句形容江门秋天的诗」也吞掉
        for c in self.theme.get("chitchat", []):
            for k in c.get("k", []):
                if k in raw and len(raw) <= len(k) + 8:
                    return self._pack(c.get("a", ""), "chat")

        # 2. 展品实体命中（优先最长别名）
        hit = None
        for ex in self.theme.get("exhibits", []):
            for alias in ex.get("aliases", []):
                if alias in raw and (hit is None or len(alias) > len(hit[1])):
                    hit = (ex, alias)
        if hit:
            ex = hit[0]
            if any(k in raw for k in ("介绍", "讲讲", "说说", "讲解", "听听")):
                body = ex.get("deep", "") if (deep or any(k in raw for k in ("详细", "深入", "展开"))) else ex.get("script", "")
                return self._pack(body, "explain", ex)
            faq = self._best_faq(ex, text)
            if faq:
                return self._pack(faq.get("a", ""), "explain", ex, emotion="neutral")
            # 知识库里没有这条细节（比如「碉楼有多高」）→ 交给大模型按资料回答
            reply = self.llm_answer(text, ex.get("id"), temperature=0.35, session=session)
            if reply:
                return self._pack(reply, "explain", ex)
            body = ex.get("deep", "") if (deep or any(k in raw for k in ("详细", "深入", "展开", "具体"))) else ex.get("script", "")
            return self._pack(body, "explain", ex)

        # 3. 当前聚焦展品的追问（"它""这个"指代）
        if exhibit_id:
            ex = self.exhibit(exhibit_id)
            if ex:
                faq = self._best_faq(ex, text)
                if faq:
                    return self._pack(faq.get("a", ""), "explain", ex)
                if any(k in raw for k in ("它", "这个", "这件", "还有", "继续", "然后")):
                    return self._pack(ex.get("deep", ""), "explain", ex)

        # 4. 无实体，按关键词打分
        best, score = None, 0.0
        for ex in self.theme.get("exhibits", []):
            s = _overlap(tokens, ex.get("_tokens", []))
            if s > score:
                best, score = ex, s
        if best and score >= 0.34:
            faq = self._best_faq(best, text)
            if faq:
                return self._pack(faq.get("a", ""), "explain", best)
            return self._pack(best.get("script", ""), "explain", best)

        # 5. 意图
        if any(k in raw for k in ("路线", "顺序", "怎么逛", "参观", "推荐", "先看")):
            return self._pack(self.theme.get("route", ""), "route")
        if any(k in raw for k in ("你好", "欢迎", "开始", "开场")):
            return self._pack(self.theme.get("welcome", ""), "welcome")

        # 6. 知识库之外：交给本地大模型（千问），真正「问什么都会」
        reply = self.llm_answer(text, exhibit_id, drop_redirect=True, session=session)
        if reply:
            return self._pack(reply, "chat")

        return self._pack(self.theme.get("fallback", ""), "fallback")

    def vision_answer(self, text, images, exhibit_id=None, session=None):
        """看图回答：视觉模型直接答；画面里若在某个展台，把该展品资料一起交给它。"""
        if not llm.available():
            return None
        ex = self.exhibit(exhibit_id) if exhibit_id else None
        system = (
            "你是侨乡江门展厅的 AI 讲解员小侨。先看清图片，再用讲解员的口吻回答。"
            "图片若是展厅展品或侨乡风物，就结合已知资料介绍；若与侨乡无关，如实描述、友好回应，不必硬拉回展厅。"
            "只讲看得见的内容，拿不准就说拿不准。回答不超过 90 字，不分点、不加表情、不反问。"
        )
        if ex:
            system += f"\n观众此刻站在「{ex.get('name', '展品')}」展台前，已知资料：{ex.get('deep', '')}"
        user_text = text or "看看这张图，说说这是什么"
        msgs = [{"role": "system", "content": system},
                {"role": "user", "content": user_text}]
        try:
            reply = llm.chat(msgs, num_predict=220, temperature=0.3, images=images)
        except Exception:
            return None
        reply = _clean_llm(reply, drop_redirect=True)
        if not reply:
            return None
        self._remember(self._history(session), user_text, reply)
        return reply

    def llm_answer(self, text: str, exhibit_id=None, temperature=0.65, drop_redirect=False, session=None):
        """调用本地大模型；失败就返回 None，让上层回落到知识库。

        提问涉及展品细节时用低温（0.35）保准确，纯开放闲聊用 0.65 更像聊天。
        """
        if not llm.available():
            return None
        history = self._history(session)
        msgs = [{"role": "system", "content": self._system}]
        msgs.extend(history[-6:])
        ex = self.exhibit(exhibit_id) if exhibit_id else None
        prefix = ""
        if ex:
            prefix = f"（观众正站在「{ex.get('name', '展品')}」展台前，已知资料：{ex.get('deep', '')}）"
        msgs.append({"role": "user", "content": prefix + text})
        try:
            reply = llm.chat(msgs, temperature=temperature)
        except Exception:
            return None
        reply = _clean_llm(reply, drop_redirect=drop_redirect)
        if not reply:
            return None
        self._remember(history, text, reply)
        return reply

    @staticmethod
    def _best_faq(ex, text):
        # 观众问的正好是问答库里那句原话：直接命中，别绕去大模型（也保证缓存复用）
        raw_q = _PUNCT.sub("", text or "")
        for faq in ex.get("faq", []):
            if _PUNCT.sub("", faq.get("q", "")) == raw_q:
                return faq

        # 问句里必然出现展品名，扣掉这些词，只看真正的提问点
        tokens = [t for t in _tokens(text) if t not in ex["_tokens"]]
        want = _focus(text)
        best, score = None, 0.0
        for faq in ex.get("faq", []):
            have = _focus(faq.get("q", ""))
            # 问的是「多高」，FAQ 讲的是「为什么」，分数再高也是答非所问，跳过去交给大模型
            if want and have and not (want & have):
                continue
            s = _overlap(tokens, faq["_tokens"])
            if s > score:
                best, score = faq, s
        return best if score >= 0.5 else None

    @staticmethod
    def _pack(reply, action, ex=None, emotion=None):
        if emotion is None:
            emotion = {"welcome": "happy", "chat": "happy", "explain": "neutral", "route": "neutral"}.get(action, "neutral")
            # 中文讲解词用的是全角「！」，半角/全角都算（DEF-26）
            if action == "explain" and ex and reply and re.search(r"[!！]", reply):
                emotion = "happy"
        return {
            "reply": reply,
            "action": action,
            "emotion": emotion,
            "gesture": "point" if action == "explain" else ("wave" if action == "welcome" else "idle"),
            "exhibit": ex.get("id") if ex else None,
            "exhibit_name": ex.get("name") if ex else None,
        }
