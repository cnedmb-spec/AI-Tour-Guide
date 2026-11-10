use qx_core::Theme;

fn theme_path() -> String {
    // 项目重组后 themes/ 位于 rust-rewrite/ 下：qx-core 上两级即 rust-rewrite。
    concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../themes/qiaoxiang/theme.json"
    )
    .to_string()
}

#[test]
fn loads_and_validates_real_theme() {
    let t = Theme::load(theme_path()).expect("现有主题包应可解析");
    t.validate().expect("现有主题包应通过结构校验");
    assert_eq!(t.id, "qiaoxiang");
    assert_eq!(t.assistant.name, "小侨");
    assert!(!t.palette.accent.is_empty());
}

#[test]
fn exhibit_and_kb_counts_match_python_stats() {
    let t = Theme::load(theme_path()).unwrap();
    assert_eq!(t.exhibits.len(), 6, "对应 /api/stats 的 kb_exhibits");
    assert_eq!(t.kb_answers(), 30, "对应 /api/stats 的 kb_answers");
    for e in &t.exhibits {
        assert_eq!(e.faq.len(), 3, "每个展品 3 条 FAQ: {}", e.id);
        assert!(!e.script.is_empty(), "{} 应有讲解词", e.id);
        assert!(!e.deep.is_empty(), "{} 应深度讲解词", e.id);
        assert!(!e.aliases.is_empty(), "{} 应有别名", e.id);
    }
    assert!(t.exhibit("diaolou").is_some());
    assert!(t.exhibit("chikan").is_some());
    assert!(t.exhibit("not_exist").is_none());
}

#[test]
fn rejects_incomplete_theme() {
    let empty = Theme::default();
    assert!(empty.validate().is_err(), "空主题包必须尽早失败（对应 DEF-14）");
}

#[test]
fn preserves_optional_render_fields() {
    let t = Theme::load(theme_path()).unwrap();
    let qiaopi = t.exhibit("qiaopi").expect("侨批银信应存在");
    assert!(qiaopi.model_scale.is_some(), "modelScale 应被 camelCase 映射");
    assert!(qiaopi.plaque.is_some());
    let diaolou = t.exhibit("diaolou").unwrap();
    assert!(diaolou.materials.is_some(), "材质覆盖表应透传");
}
