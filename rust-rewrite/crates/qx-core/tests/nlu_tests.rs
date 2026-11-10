use qx_core::{Guide, LlmClient, Message, Metrics, NoLlm, OllamaClient, Theme};

fn theme_path() -> String {
    // 项目重组后 themes/ 位于 rust-rewrite/ 下：qx-core 上两级即 rust-rewrite。
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
    let g = Guide::with_llm(Theme::load(theme_path()).unwrap(), Box::new(Stub));
    let a = g.answer("随便聊点别的", None, false, None);
    assert_eq!(a.action, "chat");
    assert!(!a.reply.starts_with("小侨："), "应去掉讲解员前缀");
    assert!(!a.reply.contains("**"), "应去掉 markdown 标记");
    assert!(!a.reply.contains("要不要我带你去看"), "应剪掉拉回展品的尾句");
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
