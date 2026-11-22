use qx_core::{Guide, LlmClient, Message, Metrics, NoLlm, OllamaClient, Theme};
use std::sync::{Arc, Mutex};

fn theme_path() -> String {
    // 项目重组后 themes/ 位于 qx-rust/ 下：qx-core 上两级即 qx-rust。
    concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../themes/qiaoxiang/theme.json"
    )
    .to_string()
}

fn guide() -> Guide {
    Guide::new(Theme::load(theme_path()).unwrap())
}

#[test]
fn empty_input_returns_welcome() {
    let g = guide();
    let a = g.answer("   ", None, false, None);
    assert_eq!(a.action, "welcome");
    assert_eq!(a.reply, g.theme.welcome);
    assert_eq!(a.emotion, "happy");
    assert_eq!(a.gesture, "wave");
    assert!(a.exhibit.is_none());
}

#[test]
fn realtime_questions_are_refused_offline() {
    let g = guide();
    for q in ["今天天气怎么样", "现在几点", "今天股价多少", "有什么新闻"] {
        let a = g.answer(q, None, false, None);
        assert_eq!(a.action, "fallback", "问题: {q}");
        assert!(a.reply.contains("查不到"), "问题: {q}");
    }
}

#[test]
fn alias_hit_returns_script_or_deep() {
    let g = guide();
    let a = g.answer("介绍开平碉楼", None, false, None);
    assert_eq!(a.action, "explain");
    assert_eq!(a.exhibit.as_deref(), Some("diaolou"));
    assert_eq!(a.reply, g.theme.exhibit("diaolou").unwrap().script);
    assert_eq!(a.gesture, "point");

    let b = g.answer("详细介绍开平碉楼", None, false, None);
    assert_eq!(b.reply, g.theme.exhibit("diaolou").unwrap().deep);
}

#[test]
fn exact_faq_question_is_answered_from_kb() {
    let g = guide();
    let ex = g.theme.exhibit("diaolou").unwrap();
    for f in &ex.faq {
        let a = g.answer(&f.q, None, false, None);
        assert_eq!(a.reply, f.a, "FAQ 问句: {}", f.q);
        assert_eq!(a.exhibit.as_deref(), Some("diaolou"));
    }
}

#[test]
fn faq_question_without_exhibit_name_is_still_answered() {
    // 互动问答的「问问小侨」只把 followup 原话发过来，句子里往往没有展品别名
    //（「三年是什么意思」「他的子女都有谁」）。这类问题必须能在全库问答里命中，
    // 不能掉进大模型或兜底话术——否则现场演示会出现「答不上来」。
    let g = guide();
    for ex in &g.theme.exhibits {
        for f in &ex.faq {
            let a = g.answer(&f.q, None, false, None);
            assert_eq!(a.reply, f.a, "FAQ 问句未被命中: {}（展项 {}）", f.q, ex.id);
            assert_eq!(
                a.exhibit.as_deref(),
                Some(ex.id.as_str()),
                "命中展项不符: {}",
                f.q
            );
        }
    }
}

#[test]
fn quiz_question_without_exhibit_name_is_answered() {
    // 现场复现的坑：「让小鸟天堂出了名的是哪位作家？」句子里没有「小鸟天堂」的别名，
    // 旧路由会直接落到大模型/兜底。题面本身就是知识，必须命中小鸟天堂展项并给出巴金。
    let g = guide();
    let a = g.answer("让小鸟天堂出了名的是哪位作家？", None, false, None);
    assert_eq!(a.action, "explain");
    assert_eq!(a.exhibit.as_deref(), Some("bird"), "实际回复: {}", a.reply);
    assert!(a.reply.contains("巴金"), "应点出巴金，实际: {}", a.reply);
    assert!(a.reply.contains("鸟的天堂"), "应提到《鸟的天堂》，实际: {}", a.reply);
}

#[test]
fn every_quiz_question_is_answered_from_kb() {
    // 互动问答的题目会被游客原样念出来（题面里通常带展品名，但也常只复述题面），
    // 题目不进检索就永远答不上。逐题验证：题面直接作答、落到所属展项、不落兜底。
    let g = guide();
    assert!(!g.theme.quiz.is_empty(), "主题包应带互动问答");
    for q in &g.theme.quiz {
        let expect_id = g
            .theme
            .exhibits
            .iter()
            .find(|e| e.name == q.label)
            .expect("quiz.label 应能对应到展项 name")
            .id
            .clone();
        let a = g.answer(&q.q, None, false, None);
        assert_eq!(a.action, "explain", "互动问答题面: {}", q.q);
        assert_eq!(
            a.exhibit.as_deref(),
            Some(expect_id.as_str()),
            "题面 {} 未落到所属展项，实际: {}",
            q.q,
            a.reply
        );
        assert_ne!(a.reply, g.theme.fallback, "题面 {} 落到兜底", q.q);
    }
}

#[test]
fn route_and_greeting_intents() {
    let g = guide();
    assert_eq!(g.answer("参观路线", None, false, None).action, "route");
    // 「你好」在主题包 chitchat 里，闲聊分支排在意图分支之前（与 Python 一致）
    assert_eq!(g.answer("你好", None, false, None).action, "chat");
    // 「开场」不在 chitchat 触发词表，才走到意图分支
    assert_eq!(g.answer("开场", None, false, None).action, "welcome");
}

#[test]
fn matches_python_reference_routes() {
    // 基线取自 server/nlu.py Guide.answer() 在同一主题包上的实际输出
    let g = guide();
    let cases: &[(&str, &str)] = &[
        ("", "welcome"),
        ("你好", "chat"),
        ("参观路线", "route"),
        ("介绍开平碉楼", "explain"),
        ("为什么叫碉楼", "explain"),
        ("今天天气怎么样", "fallback"),
    ];
    for (q, expected) in cases {
        assert_eq!(
            g.answer(q, None, false, None).action,
            *expected,
            "问题: {q:?}"
        );
    }
}

#[test]
fn focused_exhibit_followup_uses_deep() {
    let g = guide();
    let a = g.answer("它还有什么讲究", Some("diaolou"), false, None);
    assert_eq!(a.action, "explain");
    assert_eq!(a.exhibit.as_deref(), Some("diaolou"));
}

#[test]
fn no_llm_falls_back_instead_of_error() {
    let g = guide();
    let a = g.answer("请写一首关于量子力学的十四行诗", None, false, None);
    assert_eq!(a.action, "fallback");
    assert_eq!(a.reply, g.theme.fallback);
    assert!(!NoLlm.available());
}

#[test]
fn stub_llm_path_cleans_output() {
    struct Stub;
    impl qx_core::LlmClient for Stub {
        fn chat(&self, _: &[Message], _: f32, _: u32) -> Result<String, String> {
            // 同时命中 Python 的「重定向提示」与「侨乡话题」词表，尾句应被剪掉
            Ok("小侨：这是一段**回答**。\n要不要我带你去看侨乡的其他展品？".into())
        }
    }
    let g = Guide::with_llm(Theme::load(theme_path()).unwrap(), Arc::new(Stub));
    let a = g.answer("随便聊点别的", None, false, None);
    assert_eq!(a.action, "chat");
    assert!(!a.reply.starts_with("小侨："), "应去掉讲解员前缀");
    assert!(!a.reply.contains("**"), "应去掉 markdown 标记");
    assert!(!a.reply.contains("要不要我带你去看"), "应剪掉拉回展品的尾句");
}

#[test]
fn assistant_name_prefix_is_stripped_with_any_separator() {
    // 模型偶发带上自称前缀，分隔符可能是：、：，。或空格，都应一并剥掉。
    for raw in [
        "小侨：这是答案",
        "小侨,这是答案",
        "小侨，这是答案",
        "小侨。这是答案",
        "小侨 这是答案",
    ] {
        struct Stub(&'static str);
        impl qx_core::LlmClient for Stub {
            fn chat(&self, _: &[Message], _: f32, _: u32) -> Result<String, String> {
                Ok(self.0.into())
            }
        }
        let g = Guide::with_llm(Theme::load(theme_path()).unwrap(), Arc::new(Stub(raw)));
        let a = g.answer("随便聊点别的", None, false, None);
        assert!(
            !a.reply.starts_with("小侨"),
            "应去掉自称前缀: {raw:?} -> {}",
            a.reply
        );
        assert!(a.reply.contains("这是答案"), "应保留正文: {raw:?} -> {}", a.reply);
    }
}

#[test]
fn think_block_is_stripped_from_llm_output() {
    struct Thinking;
    impl qx_core::LlmClient for Thinking {
        fn chat(&self, _: &[Message], _: f32, _: u32) -> Result<String, String> {
            Ok("<think>用户想了解侨批，我先想一下该怎么讲……</think>\n\n\
                 侨批是海外华侨寄回家乡的信与汇款合一的家书。"
                .into())
        }
    }
    let g = Guide::with_llm(Theme::load(theme_path()).unwrap(), Arc::new(Thinking));
    let a = g.answer("随便聊点别的", None, false, None);
    assert_eq!(a.action, "chat");
    assert!(!a.reply.contains("think"), "思维链标签必须被剥离: {}", a.reply);
    assert!(!a.reply.contains("我先想一下"), "思维链内容必须被剥离: {}", a.reply);
    assert!(a.reply.contains("侨批"), "正文应保留: {}", a.reply);

    // 未闭合的思维链（被长度截断）也不能污染讲解词
    struct Truncated;
    impl qx_core::LlmClient for Truncated {
        fn chat(&self, _: &[Message], _: f32, _: u32) -> Result<String, String> {
            Ok("<think>正在思考中".into())
        }
    }
    let g2 = Guide::with_llm(Theme::load(theme_path()).unwrap(), Arc::new(Truncated));
    let a2 = g2.answer("随便聊点别的", None, false, None);
    assert!(!a2.reply.contains("think"), "未闭合标签也应被清掉: {}", a2.reply);
    assert!(!a2.reply.contains("正在思考"), "未闭合思维链内容也应被清掉: {}", a2.reply);
}

#[test]
fn image_messages_reach_vision_model() {
    struct Recorder(Arc<Mutex<Vec<Message>>>);
    impl qx_core::LlmClient for Recorder {
        fn chat(&self, msgs: &[Message], _: f32, _: u32) -> Result<String, String> {
            *self.0.lock().unwrap() = msgs.to_vec();
            Ok("画面里是开平碉楼，建于清末民初，兼具防卫与居住功能。".into())
        }
    }
    let seen = Arc::new(Mutex::new(Vec::<Message>::new()));
    let g = Guide::with_llm(
        Theme::load(theme_path()).unwrap(),
        Arc::new(Recorder(seen.clone())),
    );

    // 只拖图、不打字：必须走视觉模型，而不是被空输入分支吞成欢迎语
    let a = g.answer_with_images("", None, false, None, &["AAAA".to_string()]);
    assert_eq!(a.action, "chat");
    assert!(a.reply.contains("碉楼"), "回复应来自视觉模型: {}", a.reply);
    {
        let msgs = seen.lock().unwrap();
        assert_eq!(msgs.len(), 2);
        assert!(msgs[0].content.contains("先看清图片"), "应使用识图专用提示词");
        assert_eq!(msgs[1].images, vec!["AAAA".to_string()]);
        assert!(msgs[1].content.contains("看看这张图"));
    }

    // 纯文本问答仍不带图片，Ollama 请求体与迁移前一致
    let _ = g.answer("随便聊点别的", None, false, None);
    let msgs = seen.lock().unwrap();
    assert!(msgs.iter().all(|m| m.images.is_empty()), "文本问答不应夹带图片");
}

#[test]
fn image_request_serializes_to_ollama_protocol() {
    let plain = serde_json::to_value(Message::user("你好")).unwrap();
    assert!(plain.get("images").is_none(), "空图片数组不应出现在请求体里");

    let with = serde_json::to_value(
        Message::user("看看这张图").with_images(vec!["AAAA".to_string()]),
    )
    .unwrap();
    assert_eq!(with["images"][0], "AAAA");
    assert_eq!(with["role"], "user");
}

#[test]
fn image_only_without_vision_has_clear_fallback() {
    // 纯文本后端（本地 GGUF）：不支持视觉
    struct TextOnly;
    impl qx_core::LlmClient for TextOnly {
        fn chat(&self, _: &[Message], _: f32, _: u32) -> Result<String, String> {
            Ok("这是纯文本回答。".into())
        }
        fn supports_vision(&self) -> bool {
            false
        }
    }
    let g = Guide::with_llm(Theme::load(theme_path()).unwrap(), Arc::new(TextOnly));
    let a = g.answer_with_images("", None, false, None, &["AAAA".to_string()]);
    assert_eq!(a.action, "fallback");
    assert!(a.reply.contains("看不了"), "要说清原因: {}", a.reply);

    // 图 + 文字：视觉不可用时文字问题不能一起丢掉（DEF-20）
    let b = g.answer_with_images("随便聊点别的", None, false, None, &["AAAA".to_string()]);
    assert_eq!(b.action, "chat");
    assert!(b.reply.contains("纯文本回答"));

    // Ollama 没起（NoLlm）时 likewise 给明确降级，而不是通用兜底
    let g0 = guide();
    let c = g0.answer_with_images("", None, false, None, &["AAAA".to_string()]);
    assert_eq!(c.action, "fallback");
    assert!(c.reply.contains("看不了"), "要说清原因: {}", c.reply);
    assert!(!g0.vision_available() && !NoLlm.supports_vision());
}

#[test]
fn metrics_and_default_client() {
    let m = Metrics::default();
    m.on_request();
    m.on_request();
    m.on_llm();
    let s = m.snapshot();
    assert_eq!(s.requests, 2);
    assert_eq!(s.llm_requests, 1);
    assert_eq!(s.tts_requests, 0);

    let c = OllamaClient::default();
    assert_eq!(c.model, "qwen3.5:4b");
    assert_eq!(c.host, "http://127.0.0.1:11434");
}
