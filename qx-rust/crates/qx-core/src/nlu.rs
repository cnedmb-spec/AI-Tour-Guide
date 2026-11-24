//! 知识库检索与话术路由，行为对齐 `server/nlu.py` 的六级路由。
//!
//! 一处有意的**优化**：原版用 jieba 分词 + pypinyin 同音纠错。重写版改为
//! 「主题包领域词典最长匹配 + 汉字双字组」，短查询不再被通用分词器切错，
//! 也去掉了 jieba / pypinyin / 词典文件三项运行时依赖。别名、FAQ、实时信息、
//! 欢迎与路线等主要分支本来就走子串匹配，行为保持一致。
//!
//! 尚未移植（阶段二）：拼音同音纠错 `correct()`。会话级 LLM 上下文与带图视觉问答
//! 已在当前 Rust 链路实现。

use crate::llm::{LlmClient, Message, NoLlm};
use crate::theme::{Exhibit, Quiz, Theme};
use regex::Regex;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Instant;

const MAX_LLM_SESSIONS: usize = 128;
const MAX_LLM_HISTORY_MESSAGES: usize = 10;
const MAX_SESSION_ID_CHARS: usize = 128;

struct SessionHistory {
    messages: Vec<Message>,
    touched: Instant,
}

struct SessionState {
    history: Mutex<SessionHistory>,
}

const DEFAULT_REALTIME: &str = "我现在没能查到这类实时信息，可能是网络或数据源暂时不可用。侨乡的事您尽管问，碉楼、骑楼、陈皮、侨批我都熟。";

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Answer {
    pub reply: String,
    pub action: String,
    pub emotion: String,
    pub gesture: String,
    pub exhibit: Option<String>,
    pub exhibit_name: Option<String>,
    /// 多图问答时与图片顺序一一对应的展项 id；None 表示该图无关、应退出特写。
    /// 单图/纯文字问答为空，保持旧客户端兼容。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exhibit_sequence: Vec<Option<String>>,
}

/// 互动问答题面在检索里的形态。
#[derive(Debug, Clone)]
struct QuizEntry {
    /// 所属展项下标
    ex: usize,
    /// 题面原文
    q: String,
    /// 题面去掉标点，用于整句全等
    norm: String,
    tokens: HashSet<String>,
    /// 口语答案（由 `right` 点评与正确选项拼成）
    a: String,
}

pub struct Guide {
    pub theme: Theme,
    llm: Arc<dyn LlmClient>,
    dict: HashSet<String>,
    stop: HashSet<String>,
    ex_tokens: Vec<HashSet<String>>,
    faq_tokens: Vec<Vec<HashSet<String>>>,
    /// 互动问答题面：按展项挂好，答题面与展品 FAQ 走同一套检索。
    /// 题面是游客会原样复述的问题（「让小鸟天堂出了名的是哪位作家？」），
    /// 不进知识库就永远只能落大模型/兜底，所以必须并进检索。
    quiz: Vec<QuizEntry>,
    punct: Regex,
    realtime: Regex,
    focus_rules: Vec<(&'static str, Regex)>,
    what: Regex,
    md: Regex,
    redirect_hint: Regex,
    redirect_topic: Regex,
    session_history: Mutex<HashMap<String, Arc<SessionState>>>,
}

/// 把互动问答点评里的口语判语（「答对了！」「没错，」「对，」）去掉，
/// 只留真正讲事实的正文，作为口头答案。
fn strip_verdict(s: &str) -> String {
    let mut t = s.trim();
    for verdict in ["答对了！", "答对了，", "没错，", "对，", "对,", "是的，"] {
        if let Some(rest) = t.strip_prefix(verdict) {
            t = rest.trim();
            break;
        }
    }
    t.to_string()
}

/// 互动问答的口语答案：优先用点评正文；问的是「谁 / 哪 / 多少」这类需要先给
/// 明确答案的，把正确选项补在前面（题干「哪位作家」只补「巴金」）。
fn quiz_answer(z: &Quiz) -> String {
    let body = strip_verdict(&z.right);
    let option = z
        .options
        .get(z.answer)
        .map(|o| o.trim())
        .filter(|o| !o.is_empty());
    if let Some(o) = option {
        if !body.is_empty() && !body.contains(o) {
            // 句中问「谁 / 哪 / 多少 / 什么时候」才需要先亮答案
            if z.q.contains('谁')
                || z.q.contains('哪')
                || z.q.contains("多少")
                || z.q.contains("几")
            {
                return format!("{o}，{body}");
            }
        }
        if body.is_empty() {
            return o.to_string();
        }
    }
    body
}

fn rx(pattern: &str) -> Regex {
    Regex::new(pattern).expect("内置正则应为常量且合法")
}

fn char_len(s: &str) -> usize {
    s.chars().count()
}

fn overlap(a: &HashSet<String>, b: &HashSet<String>) -> f64 {
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    a.intersection(b).count() as f64 / a.len() as f64
}

impl Guide {
    pub fn new(theme: Theme) -> Self {
        Self::with_llm(theme, Arc::new(NoLlm))
    }

    pub fn with_llm(theme: Theme, llm: Arc<dyn LlmClient>) -> Self {
        let punct = rx(r"[，。！？、；：,.!?;:\s“”‘’()（）《》【】]");
        let realtime = rx(
            "天气|气温|下雨|降雨|台风|温度几度|空气质量|雾霾|新闻|热搜|头条|最近发生|刚刚发生|股价|股票|汇率|比特币|基金|油价|房价|现在几点|今天几号|今天星期几|几月几号|现在时间|比分|赛况|航班|高铁票|路况|堵不堵",
        );
        let focus_rules = vec![
            ("why", rx("为什么|为啥|原因|怎么会")),
            (
                "howmany",
                rx("多少|多高|多长|多大|多远|多久|几年|几座|几个|几次|几层|多重|几岁"),
            ),
            ("when", rx("哪年|什么时候|何时|几几年|哪个朝代|哪一代")),
            ("who", rx("是谁|谁住|谁盖|谁建|哪个人|什么人|有哪些人")),
            ("where", rx("在哪|哪里|哪儿|什么地方")),
            ("how", rx("怎么|如何|怎样")),
        ];
        let stop: HashSet<String> = "的 了 是 在 我 你 他 它 这 那 有 和 与 就 都 也 吗 呢 啊 吧 请问 一下 什么 怎么 为什么 能 可以 想 要 个 件 些 谁"
            .split_whitespace()
            .map(str::to_string)
            .collect();

        let mut dict = HashSet::new();
        for e in &theme.exhibits {
            for w in e.aliases.iter().chain(e.keywords.iter()) {
                if char_len(w) >= 2 {
                    dict.insert(w.clone());
                }
            }
        }
        for c in &theme.chitchat {
            for k in &c.k {
                if char_len(k) >= 2 {
                    dict.insert(k.clone());
                }
            }
        }

        let mut g = Self {
            theme,
            llm,
            dict,
            stop,
            ex_tokens: Vec::new(),
            faq_tokens: Vec::new(),
            quiz: Vec::new(),
            punct,
            realtime,
            focus_rules,
            what: rx("是什么|什么是|啥|什么意思"),
            md: rx(r"[*#`>~|]+"),
            redirect_hint: rx(
                "感兴趣|想不想|要不要|可以问我|还可以问|换个话题|关系不大|有点远|带你去看",
            ),
            redirect_topic: rx("侨乡|江门|碉楼|骑楼|展品|展厅|小鸟天堂|陈皮|侨批"),
            session_history: Mutex::new(HashMap::new()),
        };

        // 预计算检索词，等价于 Python Guide.load() 里构建 _tokens 的做法
        for e in &g.theme.exhibits {
            let mut set = HashSet::new();
            for w in e.keywords.iter().chain(e.aliases.iter()) {
                set.extend(g.tokenize(w));
            }
            g.ex_tokens.push(set);
            let mut faqs = Vec::new();
            for f in &e.faq {
                faqs.push(g.tokenize(&f.q));
            }
            g.faq_tokens.push(faqs);
        }

        // 互动问答题面并进检索：题面是游客会原样复述的问题（如
        // 「让小鸟天堂出了名的是哪位作家？」），不挂进检索就只能落大模型/兜底。
        let mut quiz: Vec<QuizEntry> = Vec::new();
        for z in &g.theme.quiz {
            let ex = g
                .theme
                .exhibits
                .iter()
                .position(|e| e.name == z.label || e.aliases.iter().any(|a| a == &z.label));
            let Some(ex) = ex else { continue };
            let q = z.q.trim();
            if q.is_empty() {
                continue;
            }
            let a = quiz_answer(z);
            if a.is_empty() {
                continue;
            }
            quiz.push(QuizEntry {
                ex,
                q: q.to_string(),
                norm: g.punct.replace_all(q, "").to_string(),
                tokens: g.tokenize(q),
                a,
            });
        }
        g.quiz = quiz;
        g
    }

    fn tokenize(&self, text: &str) -> HashSet<String> {
        let clean = self.punct.replace_all(text, "").to_string();
        let chars: Vec<char> = clean.chars().collect();
        let mut out = HashSet::new();
        let mut i = 0;
        while i < chars.len() {
            let mut matched = false;
            for len in (2..=5).rev() {
                if i + len <= chars.len() {
                    let cand: String = chars[i..i + len].iter().collect();
                    if self.dict.contains(&cand) {
                        if !self.stop.contains(&cand) {
                            out.insert(cand);
                        }
                        i += len;
                        matched = true;
                        break;
                    }
                }
            }
            if !matched {
                if i + 2 <= chars.len() {
                    let bg: String = chars[i..i + 2].iter().collect();
                    if !self.stop.contains(&bg) {
                        out.insert(bg);
                    }
                }
                i += 1;
            }
        }
        out
    }

    fn focus(&self, text: &str) -> HashSet<&'static str> {
        let mut out = HashSet::new();
        for (name, re) in &self.focus_rules {
            if re.is_match(text) {
                out.insert(*name);
            }
        }
        if out.is_empty() && self.what.is_match(text) {
            out.insert("what");
        }
        out
    }

    /// 对应 Python `_best_faq`：先精确命中原问句，再按疑问焦点过滤后打分。
    fn best_faq(&self, ex_idx: usize, text: &str) -> Option<String> {
        let ex = &self.theme.exhibits[ex_idx];
        let raw_q = self.punct.replace_all(text, "").to_string();
        for f in &ex.faq {
            if self.punct.replace_all(&f.q, "").to_string() == raw_q {
                return Some(f.a.clone());
            }
        }
        // 同展项的互动问答题面，题面也允许整句全等
        for z in self.quiz.iter().filter(|z| z.ex == ex_idx) {
            if z.norm == raw_q {
                return Some(z.a.clone());
            }
        }
        let mut tokens = self.tokenize(text);
        for t in &self.ex_tokens[ex_idx] {
            tokens.remove(t);
        }
        let want = self.focus(text);
        let mut best: Option<(f64, String)> = None;
        for (j, f) in ex.faq.iter().enumerate() {
            let have = self.focus(&f.q);
            if !want.is_empty() && !have.is_empty() && want.intersection(&have).next().is_none() {
                continue;
            }
            let s = overlap(&tokens, &self.faq_tokens[ex_idx][j]);
            if best.as_ref().is_none_or(|(bs, _)| s > *bs) {
                best = Some((s, f.a.clone()));
            }
        }
        for z in self.quiz.iter().filter(|z| z.ex == ex_idx) {
            let have = self.focus(&z.q);
            if !want.is_empty() && !have.is_empty() && want.intersection(&have).next().is_none() {
                continue;
            }
            let s = overlap(&tokens, &z.tokens);
            if best.as_ref().is_none_or(|(bs, _)| s > *bs) {
                best = Some((s, z.a.clone()));
            }
        }
        best.filter(|(s, _)| *s >= 0.5).map(|(_, a)| a)
    }

    /// 全库 FAQ 检索：问题里没有展品别名时（「三年是什么意思」「他的子女都有谁」
    /// 这类由互动问答的「问问小侨」抛出来的追问），原来的流程永远到不了对应展项，
    /// 只能落进大模型或兜底话术。这里先在全部展项的问答里按问句本身找最像的一条。
    fn best_faq_global(&self, text: &str) -> Option<(usize, String)> {
        let raw_q = self.punct.replace_all(text, "").to_string();
        for (i, ex) in self.theme.exhibits.iter().enumerate() {
            for f in &ex.faq {
                if self.punct.replace_all(&f.q, "").to_string() == raw_q {
                    return Some((i, f.a.clone()));
                }
            }
        }
        let tokens = self.tokenize(text);
        if tokens.is_empty() {
            return None;
        }
        let want = self.focus(text);
        let mut best: Option<(usize, f64, String)> = None;
        for (i, ex) in self.theme.exhibits.iter().enumerate() {
            for (j, f) in ex.faq.iter().enumerate() {
                let have = self.focus(&f.q);
                if !want.is_empty() && !have.is_empty() && want.intersection(&have).next().is_none()
                {
                    continue;
                }
                let s = overlap(&tokens, &self.faq_tokens[i][j]);
                if s >= 0.6 && best.as_ref().is_none_or(|(_, bs, _)| s > *bs) {
                    best = Some((i, s, f.a.clone()));
                }
            }
        }
        // 互动问答题面与展品 FAQ 同等参与全库检索
        for z in &self.quiz {
            let have = self.focus(&z.q);
            if !want.is_empty() && !have.is_empty() && want.intersection(&have).next().is_none() {
                continue;
            }
            let s = overlap(&tokens, &z.tokens);
            if s >= 0.6 && best.as_ref().is_none_or(|(_, bs, _)| s > *bs) {
                best = Some((z.ex, s, z.a.clone()));
            }
        }
        best.map(|(i, _, a)| (i, a))
    }

    pub fn answer(
        &self,
        text: &str,
        exhibit_id: Option<&str>,
        deep: bool,
        session: Option<&str>,
    ) -> Answer {
        self.answer_with_images(text, exhibit_id, deep, session, &[])
    }

    /// 带图问答入口：前端把压缩后的图片 base64 放进 `images`，这里优先交给视觉模型。
    /// 迁移到 Rust 版时整条 images 链路曾被漏掉，导致拖图后只走文字兜底（「答不上来」）。
    pub fn answer_with_images(
        &self,
        text: &str,
        exhibit_id: Option<&str>,
        deep: bool,
        session: Option<&str>,
        images: &[String],
    ) -> Answer {
        if !images.is_empty() {
            if let Some((reply, ex_idx, sequence)) = self.vision_answer(text, images) {
                // 展项必须由「图片内容」决定，不能沿用前端传来的 exhibit_id。
                // 多图时 sequence 与图片一一对应，供前端依次切换；exhibit 仍保留第一个
                // 命中的展项，供旧客户端和问答记录使用。
                let action = if ex_idx.is_some() { "explain" } else { "chat" };
                let mut answer = self.pack(reply, action, ex_idx, None);
                answer.exhibit_sequence = sequence
                    .iter()
                    .map(|idx| idx.map(|i| self.theme.exhibits[i].id.clone()))
                    .collect();
                return answer;
            }
            if text.trim().is_empty() {
                // 只发了图、而后端没能给出答案：说清原因，别丢一句不相干的兜底话术。
                // 注意 available() 对 Ollama 客户端是「假定可用」，真实调用失败时
                // vision_available() 仍可能为 true，故按是否有视觉能力分两种措辞。
                let msg = if !self.vision_available() {
                    "这张图我暂时看不了：当前模型不支持看图，或本地大模型还没连上。您可以直接用文字问我，我来介绍展厅里的展品。"
                } else {
                    "这张图我暂时看不了，可能是本地大模型正忙或还没连上。请再试一次，也可以直接用文字问我。"
                };
                return self.pack(msg.to_string(), "fallback", None, Some("neutral"));
            }
            // 随图还写了文字：别把文字问题一起丢掉，继续走普通文本问答（对齐旧版 DEF-20）
        }
        self.answer_text(text, exhibit_id, deep, session)
    }

    fn answer_text(
        &self,
        text: &str,
        exhibit_id: Option<&str>,
        deep: bool,
        session: Option<&str>,
    ) -> Answer {
        let text = text.trim();
        let raw = self.punct.replace_all(text, "").to_string();

        // 0. 空输入 / 实时信息
        if raw.is_empty() {
            return self.pack(self.theme.welcome.clone(), "welcome", None, Some("happy"));
        }
        if self.realtime.is_match(&raw) {
            let msg = if self.theme.realtime.is_empty() {
                DEFAULT_REALTIME.to_string()
            } else {
                self.theme.realtime.clone()
            };
            return self.pack(msg, "fallback", None, Some("neutral"));
        }

        // 1. 闲聊：只认「整句就是在聊这个」，避免吞掉长句
        for c in &self.theme.chitchat {
            for k in &c.k {
                if raw.contains(k) && char_len(&raw) <= char_len(k) + 8 {
                    return self.pack(c.a.clone(), "chat", None, None);
                }
            }
        }

        // 2. 展品实体命中（优先最长别名）
        let mut hit: Option<(usize, usize)> = None;
        for (i, ex) in self.theme.exhibits.iter().enumerate() {
            for a in &ex.aliases {
                let n = char_len(a);
                if raw.contains(a) && hit.is_none_or(|(_, best)| n > best) {
                    hit = Some((i, n));
                }
            }
        }
        if let Some((idx, _)) = hit {
            let ex = self.theme.exhibits[idx].clone();
            let intro = ["介绍", "讲讲", "说说", "讲解", "听听"]
                .iter()
                .any(|k| raw.contains(k));
            if intro {
                let want_deep = deep || ["详细", "深入", "展开"].iter().any(|k| raw.contains(k));
                let body = if want_deep {
                    ex.deep.clone()
                } else {
                    ex.script.clone()
                };
                return self.pack(body, "explain", Some(idx), None);
            }
            if let Some(a) = self.best_faq(idx, text) {
                return self.pack(a, "explain", Some(idx), Some("neutral"));
            }
            if let Some(reply) = self.llm_answer(text, Some(&ex), 0.35, false, session) {
                return self.pack(reply, "explain", Some(idx), None);
            }
            let want_deep = deep
                || ["详细", "深入", "展开", "具体"]
                    .iter()
                    .any(|k| raw.contains(k));
            let body = if want_deep {
                ex.deep.clone()
            } else {
                ex.script.clone()
            };
            return self.pack(body, "explain", Some(idx), None);
        }

        // 3. 当前聚焦展品的追问（「它」「这个」指代）
        if let Some(id) = exhibit_id {
            if let Some(idx) = self.theme.exhibits.iter().position(|e| e.id == id) {
                if let Some(a) = self.best_faq(idx, text) {
                    return self.pack(a, "explain", Some(idx), None);
                }
                if ["它", "这个", "这件", "还有", "继续", "然后"]
                    .iter()
                    .any(|k| raw.contains(k))
                {
                    let body = self.theme.exhibits[idx].deep.clone();
                    return self.pack(body, "explain", Some(idx), None);
                }
            }
        }

        // 4. 无实体：先按问句在整库问答里找（互动问答的追问多走这条路），再按关键词打分
        if let Some((idx, a)) = self.best_faq_global(text) {
            return self.pack(a, "explain", Some(idx), None);
        }
        let tokens = self.tokenize(text);
        let mut best: Option<(usize, f64)> = None;
        for (i, ex_tok) in self.ex_tokens.iter().enumerate() {
            let s = overlap(&tokens, ex_tok);
            if best.is_none_or(|(_, bs)| s > bs) {
                best = Some((i, s));
            }
        }
        if let Some((idx, score)) = best {
            if score >= 0.34 {
                let reply = self
                    .best_faq(idx, text)
                    .unwrap_or_else(|| self.theme.exhibits[idx].script.clone());
                return self.pack(reply, "explain", Some(idx), None);
            }
        }

        // 5. 意图
        if ["路线", "顺序", "怎么逛", "参观", "推荐", "先看"]
            .iter()
            .any(|k| raw.contains(k))
        {
            return self.pack(self.theme.route.clone(), "route", None, None);
        }
        if ["你好", "欢迎", "开始", "开场"]
            .iter()
            .any(|k| raw.contains(k))
        {
            return self.pack(self.theme.welcome.clone(), "welcome", None, None);
        }

        // 6. 知识库之外交给本地大模型，失败回落到兜底话术
        if let Some(reply) = self.llm_answer(text, None, 0.65, true, session) {
            return self.pack(reply, "chat", None, None);
        }
        self.pack(self.theme.fallback.clone(), "fallback", None, None)
    }

    /// 把整个主题包知识库（展品讲解词 + 深度资料 + 问答对 + 路线）原样喂给模型，
    /// 让任何一轮问答都基于已知资料、不臆测。KB 体量很小（6 个展项），直接塞进
    /// system prompt 即可，无需向量库 / RAG 检索（朋友建议的「把数据库以提示词的方式
    /// 喂给 AI」）。注意：只放事实资料，不放 quiz 答案与兜底话术，避免模型直接背答案。
    fn kb_context(&self) -> String {
        let mut s = String::from(
            "## 已知资料（只能依据以下资料作答，资料里没有就如实说不知道，绝不编造）\n",
        );
        for e in &self.theme.exhibits {
            s.push_str(&format!("【{}】\n{}\n{}\n", e.name, e.script, e.deep));
            for f in &e.faq {
                s.push_str(&format!("问：{}　答：{}\n", f.q, f.a));
            }
        }
        if !self.theme.route.is_empty() {
            s.push_str(&format!("参观路线建议：{}\n", self.theme.route));
        }
        s
    }

    fn system_prompt(&self) -> String {
        format!(
            // 自我介绍口径：只报「AI 智能讲解员{名}」，前面不带「{馆名}展厅的」之类前缀
            //（用户明确要求）。同时限定话题边界：只答展馆相关内容，避免像「买围巾」
            // 这类离题编造（比赛现场事故点）。
            "你是 AI 智能讲解员{name}，在{place}为观众讲解。自我介绍时只说「我是 AI 智能讲解员{name}」，\
             不要在前面加「{place}展厅的」等前缀。\
             只谈与{place}的展品、历史、人物、风物和参观相关的内容；观众问别的事\
             （购物、点餐、私人情感、代写文章等），就礼貌说明自己只负责展馆讲解，\
             并把话题引回展品，绝不编造与展馆无关的故事。\
             只用已知资料回答，资料里没有就如实说明，不臆测、不反问。\
             回答不超过 90 字，口语化、不分点、不加表情，像面对面给游客讲解。\n\n{kb}",
            name = self.theme.assistant.name,
            place = self.theme.name,
            kb = self.kb_context(),
        )
    }

    /// 模型是否可用（决定是否走 LLM、以及 /api/stats 的 available 字段）。
    pub fn llm_available(&self) -> bool {
        self.llm.available()
    }

    /// 当前后端能否看图。Ollama 的 qwen3.5:4b 带视觉投影器；纯文本 GGUF 不能。
    pub fn vision_available(&self) -> bool {
        self.llm.available() && self.llm.supports_vision()
    }

    /// 看图回答：视觉模型直接看图，知识库只作为识别参考（对齐迁移前 Python 版语义）。
    /// system 口径与纯文本问答不同——允许如实描述与侨乡无关的画面，不硬拉回展厅。
    /// 返回的展项索引来自「图片内容识别结果」（模型输出的展项标记），不是调用方传入的
    /// 当前聚焦展项；识别不到任何展项时索引为 None，前端据此不放大任何展项。
    fn vision_answer(
        &self,
        text: &str,
        images: &[String],
    ) -> Option<(String, Option<usize>, Vec<Option<usize>>)> {
        if !self.vision_available() {
            return None;
        }
        // 展厅展项以「id（名称）」列出，让模型能按 id 输出标记。
        let exhibit_list: Vec<String> = self
            .theme
            .exhibits
            .iter()
            .map(|e| format!("{}（{}）", e.id, e.name))
            .collect();
        let mut system = format!(
            "你是{}的 AI 讲解员{}。先看清图片，再用讲解员的口吻回答。\
             图片若是展厅展品或侨乡风物，就结合已知资料介绍；若与侨乡无关，如实描述、友好回应，不必硬拉回展厅。\
             若提供了多张图片，请逐张描述，格式为「第一张……；第二张……」，不要遗漏、不要只描述其中一张。\
             只讲看得见的内容，拿不准就说拿不准。回答不超过 90 字（多张图逐张描述时可放宽到 160 字），不分点、不加表情、不反问。",
            self.theme.name, self.theme.assistant.name,
        );
        if !exhibit_list.is_empty() {
            system.push_str(&format!("\n展厅展项：{}。", exhibit_list.join("、")));
            system.push_str(
                "\n若图片内容明确是其中某个展项，就在回答最后另起一行输出标记【展项:id】（id 用上面列出的英文 id，如【展项:diaolou】）；\
                 图片内容与任何展项都无关、或看不清时，在回答最后另起一行输出【展项:none】。标记不要出现在回答正文里。",
            );
        }
        // 注意：刻意不把调用方的 exhibit_id（当前聚焦展项）写进提示词——那会把模型
        // 往旧展项上带，是"拍什么图都放大梁启超故居"的次要诱因。
        // 多图：先对每张图单独识别，得到与图片顺序一一对应的展项序列，
        // 前端据此逐张切换；第一个命中的展项同时作为 answer.exhibit，兼容旧客户端。
        // 这一步必须优先于长描述调用，且结果不允许被模型在长描述里乱写的标记覆盖。
        let (multi_ex, multi_seq) = if images.len() > 1 {
            let seq = self.classify_exhibit_sequence(images);
            let ex = seq.iter().flatten().next().copied();
            (ex, seq)
        } else {
            (None, Vec::new())
        };
        let user_text = if text.trim().is_empty() {
            if images.len() > 1 {
                "看看这几张图，逐张说说每张是什么"
            } else {
                "看看这张图，说说这是什么"
            }
        } else {
            text
        };
        let msgs = vec![
            Message::system(system),
            Message::user(user_text).with_images(images.to_vec()),
        ];
        let raw = match self.llm.chat(&msgs, 0.3, 220) {
            Ok(raw) => raw,
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    images = images.len(),
                    "视觉模型调用失败，降级为无视觉回答"
                );
                return None;
            }
        };
        // 先从原始回答里解析并剥离展项标记，再清洗正文，避免标记混进字幕。
        let (body, marker_idx) = self.extract_exhibit_marker(&raw);
        // 多图特写以逐图识别序列为准（上面已先算好），不受长描述里可能写乱的标记影响；
        // 单图仍用标记/正文兜底。
        let ex_idx = if images.len() > 1 {
            multi_ex
        } else {
            marker_idx
        };
        let cleaned = self.clean_llm(&body, true, if images.len() > 1 { 160 } else { 90 });
        (!cleaned.is_empty()).then_some((cleaned, ex_idx, multi_seq))
    }

    /// 对每张图单独发一次极短识别请求（只回答展项 id 或 none），
    /// 返回与图片顺序一一对应的展项索引序列；识别失败或无关的图为 None。
    fn classify_exhibit_sequence(&self, images: &[String]) -> Vec<Option<usize>> {
        let list: Vec<String> = self
            .theme
            .exhibits
            .iter()
            .map(|e| format!("{}（{}）", e.id, e.name))
            .collect();
        let system = format!(
            "你是展厅图片识别器。展厅展项：{}。\
             看图后只回答图片对应的英文 id（如 diaolou）；\
             与展项无关或看不清时只回答 none。不要输出任何其他文字。",
            list.join("、")
        );
        let mut sequence: Vec<Option<usize>> = Vec::with_capacity(images.len());
        for (i, image) in images.iter().enumerate() {
            let msgs = vec![
                Message::system(system.clone()),
                Message::user("识别这张图。只回答 id 或 none。").with_images(vec![image.clone()]),
            ];
            match self.llm.chat(&msgs, 0.0, 24) {
                Ok(token) => {
                    let token = Self::normalize_classify_token(&token);
                    let idx = if token.is_empty() || token.eq_ignore_ascii_case("none") {
                        None
                    } else {
                        // 单图识别只接受精确的 id / 展项名称 / 别名。不能用 contains：
                        // 模型偶尔返回长句，若其中出现「梁启超」就会被误判成 liang。
                        self.match_exhibit_exact_token(&token)
                    };
                    if let Some(idx) = idx {
                        tracing::info!(index = i, exhibit = %self.theme.exhibits[idx].id, "多图识别命中展项");
                    }
                    sequence.push(idx);
                }
                Err(e) => {
                    tracing::warn!(error = %e, index = i, "单图识别请求失败，跳过");
                    sequence.push(None);
                }
            }
        }
        sequence
    }

    /// 从视觉模型回答里摘出全部【展项:id】标记：返回（去掉所有标记后的正文, 命中的展项索引）。
    /// 多图回答可能出现多个标记（无关图 none、展项图 id）；只要有一个展项标记就按
    /// 最后一个展项标记切换（与"最后一张认出的展项"一致）；全为 none 才视为无关。
    /// 标记里的 token 既接受英文 id，也接受中文名称/别名，兼容模型不严格照做的情况。
    fn extract_exhibit_marker(&self, raw: &str) -> (String, Option<usize>) {
        const KEY: &str = "【展项";
        let mut body = raw.to_string();
        let mut markers: Vec<Option<usize>> = Vec::new();
        let mut marker_count = 0usize;
        let search_from = 0usize;
        // 循环找所有标记：模型多图时常输出【展项:none】【展项:liang】串
        while let Some(rel) = body[search_from..].find(KEY) {
            let start = search_from + rel;
            let after_key = &body[start + KEY.len()..];
            // 容许中英文冒号与多余空格：小模型常写成【展项：diaolou】
            let token_start = after_key
                .find(|c: char| !c.is_whitespace() && c != ':' && c != '：')
                .unwrap_or(0);
            let after_colon = &after_key[token_start..];
            let Some(rel_end) = after_colon.find('】') else {
                break;
            };
            let token = after_colon[..rel_end].trim();
            marker_count += 1;
            let explicit_none = matches!(
                token.to_ascii_lowercase().as_str(),
                "none" | "null" | "无" | "无关" | "没有"
            );
            markers.push(if explicit_none {
                None
            } else {
                self.match_exhibit_token(token)
            });
            // 连同整段标记一起从正文移除；search_from 不动，继续找下一个标记
            let end = start + KEY.len() + token_start + rel_end + '】'.len_utf8();
            body.replace_range(start..end, "");
        }
        // 有任何展项标记 → 按最后一个展项标记切换；none 只在没有任何展项标记时生效
        let mut idx = markers.iter().flatten().next_back().copied();
        // 兜底：模型完全没输出标记时，直接从正文里按展项名称/别名匹配。
        // 覆盖「文字里认出了展项（如『赤坎古镇的骑楼』）但没输出标记」的情况。
        // 但只要模型输出过标记（哪怕全是 none），就尊重模型结论，不再从正文捞。
        if marker_count == 0 {
            idx = self.match_exhibit_in_text(raw);
        }
        (body, idx)
    }

    /// 从正文文本中按展项名称/别名匹配（最长别名优先），作为标记缺失时的兜底。
    /// 必须排除否定/无关语境：例如「这不是赤坎古镇，而是一只猫」不能命中赤坎；
    /// 「不是赤坎古镇，而是梁启超的故居」应命中梁启超，而不是被较长的「赤坎古镇」抢走。
    fn match_exhibit_in_text(&self, text: &str) -> Option<usize> {
        let mut hit: Option<(usize, usize)> = None;
        for (i, e) in self.theme.exhibits.iter().enumerate() {
            for alias in e.aliases.iter().chain(std::iter::once(&e.name)) {
                let mut from = 0;
                while let Some(rel) = text[from..].find(alias.as_str()) {
                    let start = from + rel;
                    let end = start + alias.len();
                    if !Self::alias_is_negated(text, start, end) {
                        let n = char_len(alias);
                        if hit.is_none_or(|(_, best)| n > best) {
                            hit = Some((i, n));
                        }
                    }
                    from = end;
                }
            }
        }
        hit.map(|(i, _)| i)
    }

    /// 判断别名这次出现是否处于否定/无关语境。只看邻近短窗，避免误伤正常讲解。
    fn alias_is_negated(text: &str, start: usize, end: usize) -> bool {
        let before = text[..start]
            .chars()
            .rev()
            .take(8)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect::<String>();
        let after = text[end..].chars().take(6).collect::<String>();
        let neg_before = ["不是", "并非", "不像", "不属于", "没有", "无关", "非"];
        let neg_after = ["无关", "不相干", "不符", "不像"];
        neg_before.iter().any(|k| before.ends_with(k))
            || neg_after.iter().any(|k| after.starts_with(k))
    }

    /// 规范化单图识别输出：取首行，去掉常见包裹符号与句尾标点。
    fn normalize_classify_token(raw: &str) -> String {
        raw.lines()
            .next()
            .unwrap_or("")
            .trim()
            .trim_matches(|c: char| {
                c.is_whitespace()
                    || c.is_ascii_punctuation()
                    || matches!(c, '。' | '，' | '；' | '：' | '【' | '】' | '（' | '）')
            })
            .to_string()
    }

    /// 单图识别专用：只接受精确的英文 id、展项名称或别名（id 大小写不敏感）。
    fn match_exhibit_exact_token(&self, token: &str) -> Option<usize> {
        if token.is_empty() {
            return None;
        }
        if let Some(i) = self
            .theme
            .exhibits
            .iter()
            .position(|e| e.id.eq_ignore_ascii_case(token))
        {
            return Some(i);
        }
        self.theme
            .exhibits
            .iter()
            .position(|e| e.name == token || e.aliases.iter().any(|alias| alias == token))
    }

    /// 把模型给出的 token 匹配到展项索引：优先英文 id，其次名称/别名（最长匹配）。
    /// 仅用于解析【展项:...】短标记；长文本/单图识别不能用这里的 contains 逻辑。
    fn match_exhibit_token(&self, token: &str) -> Option<usize> {
        if token.is_empty() {
            return None;
        }
        if let Some(i) = self.theme.exhibits.iter().position(|e| e.id == token) {
            return Some(i);
        }
        let mut hit: Option<(usize, usize)> = None;
        for (i, e) in self.theme.exhibits.iter().enumerate() {
            for alias in e.aliases.iter().chain(std::iter::once(&e.name)) {
                let n = char_len(alias);
                if (alias == token || token.contains(alias.as_str()))
                    && hit.is_none_or(|(_, best)| n > best)
                {
                    hit = Some((i, n));
                }
            }
        }
        hit.map(|(i, _)| i)
    }

    fn llm_answer(
        &self,
        text: &str,
        ex: Option<&Exhibit>,
        temperature: f32,
        drop_redirect: bool,
        session: Option<&str>,
    ) -> Option<String> {
        if !self.llm.available() {
            return None;
        }
        let content = match ex {
            Some(e) => format!(
                "（观众正站在「{}」展台前，已知资料：{}）{}",
                e.name, e.deep, text
            ),
            None => text.to_string(),
        };
        // Serialize turns within a visitor session so two requests cannot commit history
        // out of order. Invalid/oversized IDs deliberately get stateless behavior.
        let session_state = session
            .map(str::trim)
            .filter(|id| !id.is_empty() && id.chars().count() <= MAX_SESSION_ID_CHARS)
            .map(|id| self.session_state(id));
        let _turn_guard = session_state
            .as_ref()
            .map(|state| state.history.lock().unwrap());
        let mut msgs = vec![Message::system(self.system_prompt())];
        if let Some(history) = _turn_guard.as_ref() {
            msgs.extend(history.messages.iter().cloned());
        }
        msgs.push(Message::user(content.clone()));
        let raw = match self.llm.chat(&msgs, temperature, 220) {
            Ok(raw) => raw,
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    temperature = %temperature,
                    "大模型调用失败，改走知识库兜底"
                );
                return None;
            }
        };
        let cleaned = self.clean_llm(&raw, drop_redirect, 90);
        if cleaned.is_empty() {
            return None;
        }
        if let Some(mut history) = _turn_guard {
            history.messages.push(Message::user(content));
            history.messages.push(Message {
                role: "assistant".into(),
                content: cleaned.clone(),
                images: Vec::new(),
            });
            if history.messages.len() > MAX_LLM_HISTORY_MESSAGES {
                let excess = history.messages.len() - MAX_LLM_HISTORY_MESSAGES;
                history.messages.drain(..excess);
            }
            history.touched = Instant::now();
        }
        Some(cleaned)
    }

    fn session_state(&self, id: &str) -> Arc<SessionState> {
        let mut sessions = self.session_history.lock().unwrap();
        if let Some(state) = sessions.get(id) {
            return Arc::clone(state);
        }
        if sessions.len() >= MAX_LLM_SESSIONS {
            // 不阻塞持有会话锁、正在推理的请求；被 Arc 持有的状态也不能被替换，
            // 否则同一会话可能同时拥有两份互相独立的上下文。
            let oldest = sessions
                .iter()
                .filter_map(|(id, state)| {
                    state
                        .history
                        .try_lock()
                        .ok()
                        .map(|history| (id.clone(), history.touched))
                })
                .min_by_key(|(_, touched)| *touched)
                .map(|(id, _)| id);
            if let Some(oldest) = oldest {
                sessions.remove(&oldest);
            }
        }
        let state = Arc::new(SessionState {
            history: Mutex::new(SessionHistory {
                messages: Vec::new(),
                touched: Instant::now(),
            }),
        });
        sessions.insert(id.to_string(), Arc::clone(&state));
        state
    }

    fn clean_llm(&self, text: &str, drop_redirect: bool, max_chars: usize) -> String {
        // Qwen3.5 等思考模型可能把 <think>…</think> 思维链一起吐出来。必须在去掉
        // markdown 符号之前剥离（md 正则含 `>`，先把标签削成 `<think` 就再也匹配不到了）。
        // 同时处理被长度截断、只有开头没有闭合的情况。
        let mut raw = text.to_string();
        loop {
            let Some(start) = raw.find("<think>") else {
                break;
            };
            match raw[start..].find("</think>") {
                Some(rel_end) => {
                    let end = start + rel_end + "</think>".len();
                    raw.replace_range(start..end, "");
                }
                None => {
                    raw.truncate(start);
                    break;
                }
            }
        }
        let mut s = self.md.replace_all(&raw, "").to_string();
        s = s.lines().map(str::trim).collect::<Vec<_>>().join(" ");
        let mut s = s.trim().to_string();
        // 剥离模型偶发带上的助手自称前缀（"小侨：/小侨，/小侨。/小侨 " 等，分隔符不限）。
        let name = &self.theme.assistant.name;
        if s.starts_with(name.as_str()) {
            let rest = &s[name.len()..];
            let rest = rest
                .trim_start_matches(|c: char| "：:，,。.、！!？?；; \t".contains(c))
                .trim();
            if !rest.is_empty() {
                s = rest.to_string();
            }
        }
        for p in ["讲解员：", "讲解员:"] {
            if let Some(rest) = s.strip_prefix(p) {
                s = rest.trim().to_string();
                break;
            }
        }
        if drop_redirect {
            s = self.trim_redirect(&s);
        }
        if char_len(&s) > max_chars {
            let chars: Vec<char> = s.chars().collect();
            let mut cut = None;
            for (i, c) in chars.iter().enumerate().take(90) {
                if "。！？；".contains(*c) && i as f64 > 40.5 {
                    cut = Some(i + 1);
                }
            }
            s = match cut {
                Some(k) => chars[..k].iter().collect(),
                None => format!("{}。", chars[..90].iter().collect::<String>()),
            };
        }
        s.trim().to_string()
    }

    fn trim_redirect(&self, text: &str) -> String {
        let chars: Vec<char> = text.chars().collect();
        for i in (0..chars.len()).rev() {
            if "。！？".contains(chars[i]) {
                let mut j = i;
                while j > 0 && !"。！？".contains(chars[j - 1]) {
                    j -= 1;
                }
                let last: String = chars[j..=i].iter().collect();
                if self.redirect_hint.is_match(&last) && self.redirect_topic.is_match(&last) {
                    return chars[..j].iter().collect::<String>().trim().to_string();
                }
                break;
            }
        }
        text.to_string()
    }

    fn pack(
        &self,
        reply: String,
        action: &str,
        exhibit_idx: Option<usize>,
        emotion: Option<&str>,
    ) -> Answer {
        let ex = exhibit_idx.map(|i| &self.theme.exhibits[i]);
        let mut emo = emotion.map(str::to_string).unwrap_or_else(|| {
            match action {
                "welcome" | "chat" => "happy",
                _ => "neutral",
            }
            .to_string()
        });
        if emotion.is_none() && action == "explain" && (reply.contains('!') || reply.contains('！'))
        {
            emo = "happy".into();
        }
        let gesture = match action {
            "explain" => "point",
            "welcome" => "wave",
            _ => "idle",
        };
        Answer {
            reply,
            action: action.into(),
            emotion: emo,
            gesture: gesture.into(),
            exhibit: ex.map(|e| e.id.clone()),
            exhibit_name: ex.map(|e| e.name.clone()),
            exhibit_sequence: Vec::new(),
        }
    }
}
