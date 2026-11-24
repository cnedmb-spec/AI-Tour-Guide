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
fn realtime_questions_describe_temporary_lookup_failure() {
    let g = guide();
    for q in ["今天天气怎么样", "现在几点", "今天股价多少", "有什么新闻"] {
        let a = g.answer(q, None, false, None);
        assert_eq!(a.action, "fallback", "问题: {q}");
        assert!(a.reply.contains("没能查到"), "问题: {q}");
        assert!(!a.reply.contains("没有联网"), "问题: {q}");
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
    assert!(
        a.reply.contains("鸟的天堂"),
        "应提到《鸟的天堂》，实际: {}",
        a.reply
    );
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
    assert!(
        !a.reply.contains("要不要我带你去看"),
        "应剪掉拉回展品的尾句"
    );
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
        assert!(
            a.reply.contains("这是答案"),
            "应保留正文: {raw:?} -> {}",
            a.reply
        );
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
    assert!(
        !a.reply.contains("think"),
        "思维链标签必须被剥离: {}",
        a.reply
    );
    assert!(
        !a.reply.contains("我先想一下"),
        "思维链内容必须被剥离: {}",
        a.reply
    );
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
    assert!(
        !a2.reply.contains("think"),
        "未闭合标签也应被清掉: {}",
        a2.reply
    );
    assert!(
        !a2.reply.contains("正在思考"),
        "未闭合思维链内容也应被清掉: {}",
        a2.reply
    );
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
    // 正文认出了「开平碉楼」，兜底逻辑应把它切成讲解态并聚焦该展项
    assert_eq!(a.action, "explain");
    assert_eq!(a.exhibit.as_deref(), Some("diaolou"));
    assert!(a.reply.contains("碉楼"), "回复应来自视觉模型: {}", a.reply);
    {
        let msgs = seen.lock().unwrap();
        assert_eq!(msgs.len(), 2);
        assert!(
            msgs[0].content.contains("先看清图片"),
            "应使用识图专用提示词"
        );
        assert_eq!(msgs[1].images, vec!["AAAA".to_string()]);
        assert!(msgs[1].content.contains("看看这张图"));
    }

    // 纯文本问答仍不带图片，Ollama 请求体与迁移前一致
    let _ = g.answer("随便聊点别的", None, false, None);
    let msgs = seen.lock().unwrap();
    assert!(
        msgs.iter().all(|m| m.images.is_empty()),
        "文本问答不应夹带图片"
    );
}

/// 视觉模型答复末尾带的展项标记决定前端放大哪个展项；调用方传入的 exhibit_id
/// （上一次聚焦的展项）必须是无关的——这正是"拍什么图都放大梁启超故居"的根因。
#[test]
fn image_exhibit_comes_from_vision_marker_not_from_current_exhibit() {
    struct MarkerLlm(String);
    impl qx_core::LlmClient for MarkerLlm {
        fn chat(&self, _: &[Message], _: f32, _: u32) -> Result<String, String> {
            Ok(self.0.clone())
        }
    }
    let g = Guide::with_llm(
        Theme::load(theme_path()).unwrap(),
        Arc::new(MarkerLlm(
            "这是华侨出资修建的碉楼，兼具防卫与居住。\n【展项:diaolou】".into(),
        )),
    );
    // 即便前端当前聚焦的是梁启超故居，也应识别为图片里的开平碉楼
    let a = g.answer_with_images("", Some("liang"), false, None, &["AAAA".into()]);
    assert_eq!(
        a.exhibit.as_deref(),
        Some("diaolou"),
        "应放大图片识别出的展项"
    );
    assert_eq!(a.exhibit_name.as_deref(), Some("开平碉楼"));
    assert_eq!(a.action, "explain");
    assert!(!a.reply.contains("展项"), "标记不能混进字幕: {}", a.reply);
    assert!(a.reply.contains("碉楼"), "正文应保留: {}", a.reply);
}

/// 图片内容与 6 个展项都无关（或模型未给标记）时，不应放大任何展项，
/// 也不应沿用调用方传入的旧展项。
#[test]
fn image_without_matching_exhibit_does_not_focus_anything() {
    struct FixedLlm;
    impl qx_core::LlmClient for FixedLlm {
        fn chat(&self, _: &[Message], _: f32, _: u32) -> Result<String, String> {
            Ok("这是一只坐在窗台上的橘猫，阳光很好。".into())
        }
    }
    let g = Guide::with_llm(Theme::load(theme_path()).unwrap(), Arc::new(FixedLlm));
    // 传入 liang 只是模拟"用户上一次聚焦过梁启超故居"，绝不能因此放大它
    let a = g.answer_with_images("", Some("liang"), false, None, &["AAAA".into()]);
    assert!(
        a.exhibit.is_none(),
        "无关图片不应放大任何展项: {:?}",
        a.exhibit
    );
    assert!(a.exhibit_name.is_none());
    assert_eq!(a.action, "chat");
    assert!(a.reply.contains("橘猫"), "正文应保留: {}", a.reply);
}

/// 模型用中文名称/别名而非英文 id 输出标记时也应识别（容错）。
#[test]
fn image_exhibit_marker_accepts_alias() {
    struct FixedLlm;
    impl qx_core::LlmClient for FixedLlm {
        fn chat(&self, _: &[Message], _: f32, _: u32) -> Result<String, String> {
            Ok("陈皮是广东人煲汤的老底子。\n【展项:新会陈皮】".into())
        }
    }
    let g = Guide::with_llm(Theme::load(theme_path()).unwrap(), Arc::new(FixedLlm));
    let a = g.answer_with_images("", None, false, None, &["AAAA".into()]);
    assert_eq!(
        a.exhibit.as_deref(),
        Some("chenpi"),
        "中文别名也应匹配到展项"
    );
    assert!(!a.reply.contains("展项"), "标记不能混进字幕: {}", a.reply);
}

/// 小模型常把标记写成全角冒号【展项：id】，也应正确识别且不污染正文。
#[test]
fn image_exhibit_marker_accepts_fullwidth_colon() {
    struct FixedLlm;
    impl qx_core::LlmClient for FixedLlm {
        fn chat(&self, _: &[Message], _: f32, _: u32) -> Result<String, String> {
            Ok("茶坑村的青砖民居，是梁启超的故居。\n【展项：liang】".into())
        }
    }
    let g = Guide::with_llm(Theme::load(theme_path()).unwrap(), Arc::new(FixedLlm));
    let a = g.answer_with_images("", None, false, None, &["AAAA".into()]);
    assert_eq!(a.exhibit.as_deref(), Some("liang"), "全角冒号也应识别");
    assert!(!a.reply.contains("展项"), "标记不能混进字幕: {}", a.reply);
}

/// 模型正常识别出了展项、但没按格式输出标记时，也应从正文匹配并切换展项。
/// 覆盖「模型正常识别赤坎古镇，展项却不切换」这一实际现象。
#[test]
fn image_exhibit_falls_back_to_name_in_reply_when_marker_missing() {
    struct FixedLlm;
    impl qx_core::LlmClient for FixedLlm {
        fn chat(&self, _: &[Message], _: f32, _: u32) -> Result<String, String> {
            Ok("第一张是赤坎古镇的骑楼群，很有南洋风味。".into())
        }
    }
    let g = Guide::with_llm(Theme::load(theme_path()).unwrap(), Arc::new(FixedLlm));
    // 前端当前聚焦的是梁启超故居，但正文认出了赤坎古镇 → 应切到 chikan
    let a = g.answer_with_images("", Some("liang"), false, None, &["AAAA".into()]);
    assert_eq!(
        a.exhibit.as_deref(),
        Some("chikan"),
        "正文认出的展项也应切换"
    );
    assert_eq!(a.exhibit_name.as_deref(), Some("赤坎古镇"));
}

/// 多图场景：后端对每张图单独识别，返回与图片顺序一一对应的展项序列，
/// 并把第一个命中的展项作为 answer.exhibit。首张赤坎、后两张无关 → 聚焦赤坎。
#[test]
fn image_multi_shot_falls_back_to_recognized_exhibit() {
    struct SequenceLlm(Mutex<std::collections::VecDeque<String>>);
    impl qx_core::LlmClient for SequenceLlm {
        fn chat(&self, _: &[Message], _: f32, _: u32) -> Result<String, String> {
            self.0
                .lock()
                .unwrap()
                .pop_front()
                .ok_or_else(|| "没有预设回答".into())
        }
    }
    // 逐张全量识别：3 张图各 1 次，再 1 次整段描述。
    let replies = std::collections::VecDeque::from([
        "chikan".to_string(),
        "none".to_string(),
        "none".to_string(),
        "第一张是赤坎古镇的骑楼，第二张是窗台上的橘猫，第三张是一只小狗。".to_string(),
    ]);
    let g = Guide::with_llm(
        Theme::load(theme_path()).unwrap(),
        Arc::new(SequenceLlm(Mutex::new(replies))),
    );
    let a = g.answer_with_images(
        "",
        Some("liang"),
        false,
        None,
        &["A".into(), "B".into(), "C".into()],
    );
    assert_eq!(
        a.exhibit.as_deref(),
        Some("chikan"),
        "多图里认出的展项应聚焦"
    );
    assert_eq!(
        a.exhibit_sequence,
        vec![Some("chikan".to_string()), None, None],
        "展项序列应与图片顺序一一对应"
    );
}

/// 连续三张分别问答：赤坎 → 无关图片 → 梁启超故居。
/// 第二次必须取消展项，第三次必须切到梁启超；否定句里的旧展项名不能造成误判。
#[test]
fn sequential_images_switch_and_clear_exhibit() {
    struct SequenceLlm(Mutex<std::collections::VecDeque<String>>);
    impl qx_core::LlmClient for SequenceLlm {
        fn chat(&self, _: &[Message], _: f32, _: u32) -> Result<String, String> {
            self.0
                .lock()
                .unwrap()
                .pop_front()
                .ok_or_else(|| "没有预设回答".into())
        }
    }
    let replies = std::collections::VecDeque::from([
        "这是赤坎古镇的骑楼群，很有南洋风味。".to_string(),
        "这不是赤坎古镇，而是一只窗台上的橘猫。【展项:none】".to_string(),
        "这不是赤坎古镇，而是梁启超的故居。".to_string(),
    ]);
    let g = Guide::with_llm(
        Theme::load(theme_path()).unwrap(),
        Arc::new(SequenceLlm(Mutex::new(replies))),
    );

    let a1 = g.answer_with_images("", None, false, None, &["A".into()]);
    assert_eq!(a1.exhibit.as_deref(), Some("chikan"));

    let a2 = g.answer_with_images("", a1.exhibit.as_deref(), false, None, &["B".into()]);
    assert!(
        a2.exhibit.is_none(),
        "无关图必须清除旧展项: {:?}",
        a2.exhibit
    );
    assert_eq!(a2.action, "chat");
    assert!(
        !a2.reply.contains("展项"),
        "none 标记不能混进字幕: {}",
        a2.reply
    );

    let a3 = g.answer_with_images("", a2.exhibit.as_deref(), false, None, &["C".into()]);
    assert_eq!(
        a3.exhibit.as_deref(),
        Some("liang"),
        "第三张应实时切到梁启超故居"
    );
}

/// 没有 none 标记时，否定句也不能命中展项；对比句应选肯定的一侧。
#[test]
fn image_fallback_ignores_negated_exhibit_names() {
    struct FixedLlm(String);
    impl qx_core::LlmClient for FixedLlm {
        fn chat(&self, _: &[Message], _: f32, _: u32) -> Result<String, String> {
            Ok(self.0.clone())
        }
    }
    let unrelated = Guide::with_llm(
        Theme::load(theme_path()).unwrap(),
        Arc::new(FixedLlm(
            "这不是赤坎古镇，也不是梁启超故居，只是一只橘猫。".into(),
        )),
    );
    let a = unrelated.answer_with_images("", Some("chikan"), false, None, &["A".into()]);
    assert!(
        a.exhibit.is_none(),
        "否定句不应命中任何展项: {:?}",
        a.exhibit
    );

    let contrast = Guide::with_llm(
        Theme::load(theme_path()).unwrap(),
        Arc::new(FixedLlm("这不是赤坎古镇，而是梁启超的故居。".into())),
    );
    let b = contrast.answer_with_images("", Some("chikan"), false, None, &["B".into()]);
    assert_eq!(
        b.exhibit.as_deref(),
        Some("liang"),
        "应选肯定提及的梁启超故居"
    );
}

/// 多图特写以「每张单独识别、取第一张命中」为准。即便模型在长回答末尾
/// 把标记串写错（如 none+liang），也不能把第一张赤坎误切成梁启超；
/// 所有标记都要从字幕剥掉。
#[test]
fn multi_image_classification_ignores_confused_markers_and_strips_all() {
    struct SequenceLlm(Mutex<std::collections::VecDeque<String>>);
    impl qx_core::LlmClient for SequenceLlm {
        fn chat(&self, _: &[Message], _: f32, _: u32) -> Result<String, String> {
            self.0
                .lock()
                .unwrap()
                .pop_front()
                .ok_or_else(|| "没有预设回答".into())
        }
    }
    let replies = std::collections::VecDeque::from([
        "chikan".to_string(), // 第 1 张识别为赤坎
        "none".to_string(),   // 第 2 张无关
        "none".to_string(),   // 第 3 张无关
        // 长回答里模型自己写了两个标记（正是实际报错现场）
        "第一张是赤坎古镇，依河而建，建筑倒影在水中；第二张是山水风光，有竹筏和远山；第三张是梁启超故居，有雕像和砖瓦建筑。 【展项:none】【展项:liang】".to_string(),
    ]);
    let g = Guide::with_llm(
        Theme::load(theme_path()).unwrap(),
        Arc::new(SequenceLlm(Mutex::new(replies))),
    );
    let a = g.answer_with_images(
        "",
        Some("chikan"),
        false,
        None,
        &["A".into(), "B".into(), "C".into()],
    );
    assert_eq!(
        a.exhibit.as_deref(),
        Some("chikan"),
        "多图必须按图片顺序取第一张认出的展项，不能被模型标记串带偏"
    );
    assert_eq!(
        a.exhibit_sequence,
        vec![Some("chikan".to_string()), None, None],
        "展项序列不能被长回答里的标记串覆盖"
    );
    assert!(!a.reply.contains("展项"), "所有标记都要剥掉: {}", a.reply);
    assert!(a.reply.contains("梁启超故居"), "正文应保留: {}", a.reply);
}

/// 全部标记都是 none（且没有展项标记）时应视为无关，不切换。
#[test]
fn all_none_markers_clear_exhibit() {
    struct FixedLlm;
    impl qx_core::LlmClient for FixedLlm {
        fn chat(&self, _: &[Message], _: f32, _: u32) -> Result<String, String> {
            Ok("第一张是一只猫；第二张是一只狗。【展项:none】【展项:none】".into())
        }
    }
    let g = Guide::with_llm(Theme::load(theme_path()).unwrap(), Arc::new(FixedLlm));
    let a = g.answer_with_images("", Some("chikan"), false, None, &["A".into(), "B".into()]);
    assert!(a.exhibit.is_none(), "全 none 应视为无关: {:?}", a.exhibit);
    assert!(!a.reply.contains("展项"), "标记要剥掉: {}", a.reply);
}

/// 单图识别偶尔返回长句时，不能因为句中包含「梁启超」就误判成 liang；
/// 必须只接受精确 id/别名，继续识别下一张。
#[test]
fn multi_image_classifier_rejects_long_text_instead_of_matching_alias_inside_it() {
    struct SequenceLlm(Mutex<std::collections::VecDeque<String>>);
    impl qx_core::LlmClient for SequenceLlm {
        fn chat(&self, _: &[Message], _: f32, _: u32) -> Result<String, String> {
            self.0
                .lock()
                .unwrap()
                .pop_front()
                .ok_or_else(|| "没有预设回答".into())
        }
    }
    let replies = std::collections::VecDeque::from([
        "第一张是赤坎古镇，第三张是梁启超故居。".to_string(), // 非法长句，不应命中
        "chikan。".to_string(),                               // 第二张精确识别为赤坎
        "第一张是山水，第二张是赤坎古镇的骑楼。".to_string(),
    ]);
    let g = Guide::with_llm(
        Theme::load(theme_path()).unwrap(),
        Arc::new(SequenceLlm(Mutex::new(replies))),
    );
    let a = g.answer_with_images("", None, false, None, &["A".into(), "B".into()]);
    assert_eq!(a.exhibit.as_deref(), Some("chikan"));
}

#[test]
fn image_request_serializes_to_ollama_protocol() {
    let plain = serde_json::to_value(Message::user("你好")).unwrap();
    assert!(
        plain.get("images").is_none(),
        "空图片数组不应出现在请求体里"
    );

    let with =
        serde_json::to_value(Message::user("看看这张图").with_images(vec!["AAAA".to_string()]))
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
fn llm_history_is_bounded_and_isolated_by_session() {
    #[derive(Default)]
    struct RecordingLlm {
        calls: Mutex<Vec<Vec<Message>>>,
    }
    impl LlmClient for RecordingLlm {
        fn chat(&self, messages: &[Message], _: f32, _: u32) -> Result<String, String> {
            self.calls.lock().unwrap().push(messages.to_vec());
            Ok("这是讲解回答。".into())
        }
    }

    let llm = Arc::new(RecordingLlm::default());
    let guide = Guide::with_llm(Theme::load(theme_path()).unwrap(), llm.clone());
    guide.answer("随便聊聊", None, false, Some("visitor-a"));
    guide.answer("再聊一句", None, false, Some("visitor-a"));
    guide.answer("独立访客", None, false, Some("visitor-b"));
    guide.answer("无会话", None, false, None);
    guide.answer("超长会话", None, false, Some(&"x".repeat(129)));

    let calls = llm.calls.lock().unwrap();
    assert_eq!(calls.len(), 5);
    assert_eq!(calls[0].len(), 2, "首轮只含 system 和当前 user");
    assert_eq!(calls[1].len(), 4, "第二轮应带上一轮问答");
    assert_eq!(calls[1][1].content, "随便聊聊");
    assert_eq!(calls[1][3].content, "再聊一句");
    assert_eq!(calls[2].len(), 2, "其他访客不应继承上下文");
    assert_eq!(calls[3].len(), 2, "未提供 session 时保持无状态");
    assert_eq!(calls[4].len(), 2, "无效 session 不应泄漏上下文");
}

#[test]
fn llm_history_keeps_only_recent_complete_turns() {
    #[derive(Default)]
    struct RecordingLlm {
        calls: Mutex<Vec<Vec<Message>>>,
    }
    impl LlmClient for RecordingLlm {
        fn chat(&self, messages: &[Message], _: f32, _: u32) -> Result<String, String> {
            self.calls.lock().unwrap().push(messages.to_vec());
            Ok("讲解回答。".into())
        }
    }
    let llm = Arc::new(RecordingLlm::default());
    let guide = Guide::with_llm(Theme::load(theme_path()).unwrap(), llm.clone());
    for i in 0..8 {
        guide.answer(&format!("第 {i} 轮闲聊"), None, false, Some("bounded"));
    }
    let calls = llm.calls.lock().unwrap();
    assert_eq!(calls[7].len(), 12, "system + 最近 5 轮 + 当前 user");
    assert_eq!(calls[7][1].role, "user");
    assert_eq!(calls[7][1].content, "第 2 轮闲聊");
    assert_eq!(calls[7][10].role, "assistant");
    assert_eq!(calls[7][11].content, "第 7 轮闲聊");
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
