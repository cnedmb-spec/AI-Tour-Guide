//! 侨乡·江门 AI 智能讲解员 —— 独立原生桌面端（Bevy 0.20.1，对齐网页内容）
//!
//! 纯 Rust / Bevy 原生 wgpu 渲染：**不嵌网页、不用 WebView2、不启 HTTP 服务、
//! 不依赖 Python**。主题包、中文字体、展品 GLB、数字人模型全部编译期
//! `include_bytes!` 打进 exe；首次启动释放到系统临时目录，单个 exe 双击即跑。
//!
//! 内容对齐原网页：3D 展厅 + 6 展台 + VRM/GLB 数字人 + 聊天问答（输入框 / 发送 /
//! 快捷提问 / 可点展项 / 字幕 / 问答记录）+ 数字人讲解语音与口型近似。
//! 后端用 qx-core（知识库路由 + 离线问答；本机有 Ollama 时走大模型兜底）。

use bevy::{
    asset::{AssetPlugin, UnapprovedPathMode},
    ecs::{message::MessageReader, system::Local},
    gltf::GltfAssetLabel,
    input::{ButtonInput, ButtonState, keyboard::KeyboardInput},
    light::GlobalAmbientLight,
    prelude::*,
    text::{FontSource, FontSize, TextBackgroundColor},
    ui::Interaction,
    window::{WindowPlugin, WindowResolution},
};
use qx_core::{
    camera::{focus_view, CameraPath, V3},
    Guide, LlmClient, NoLlm, OllamaClient, Theme,
};
use std::{collections::HashSet, fs, net::TcpStream, path::PathBuf, time::Duration};

/// 内嵌资产释放目录；改版本号可强制刷新缓存。
const ASSET_DIR: &str = "qx-guide-native-assets-v2";

/// 把编译期内嵌的字体与模型释放到临时目录，返回 Bevy 的资产根。
fn extract_embedded_assets() -> PathBuf {
    let dir = std::env::temp_dir().join(ASSET_DIR);
    let _ = fs::create_dir_all(dir.join("fonts"));
    let _ = fs::create_dir_all(dir.join("models"));
    let files: &[(&str, &[u8])] = &[
        ("fonts/simhei.ttf", include_bytes!("../../../bevy-assets/fonts/simhei.ttf")),
        ("models/diaolou.glb", include_bytes!("../../../bevy-assets/models/diaolou.glb")),
        ("models/chikan.glb", include_bytes!("../../../bevy-assets/models/chikan.glb")),
        ("models/liang.glb", include_bytes!("../../../bevy-assets/models/liang.glb")),
        ("models/qiaopi.glb", include_bytes!("../../../bevy-assets/models/qiaopi.glb")),
        ("models/avatar.glb", include_bytes!("../../../bevy-assets/models/avatar.glb")),
    ];
    for &(rel, data) in files {
        let p = dir.join(rel);
        let stale = fs::metadata(&p)
            .map(|m| m.len() != data.len() as u64)
            .unwrap_or(true);
        if stale {
            let _ = fs::write(&p, data);
        }
    }
    dir
}

fn embedded_theme() -> Theme {
    let theme: Theme = serde_json::from_str(include_str!("../../../themes/qiaoxiang/theme.json"))
        .expect("内嵌主题包解析失败");
    theme.validate().expect("内嵌主题包校验失败");
    theme
}

fn make_guide(theme: Theme) -> Guide {
    let addr = "127.0.0.1:11434".parse().unwrap();
    let llm: Box<dyn LlmClient> = if TcpStream::connect_timeout(&addr, Duration::from_millis(200)).is_ok() {
        Box::new(OllamaClient::default())
    } else {
        Box::new(NoLlm)
    };
    Guide::with_llm(theme, llm)
}

// ───────────────────────────── 资源与标记 ─────────────────────────────

#[derive(Resource)]
struct Hall {
    theme: Theme,
    guide: Guide,
}

#[derive(Resource)]
struct Cam {
    base: V3,
    base_look: V3,
    current: V3,
    current_look: V3,
    path: Option<CameraPath>,
    elapsed: f32,
    duration: f32,
    focus: Option<usize>,
}

impl Cam {
    fn new() -> Self {
        let base = V3::new(0.0, 1.5, 6.0);
        let base_look = V3::new(0.0, 1.0, -1.2);
        Self {
            base,
            base_look,
            current: base,
            current_look: base_look,
            path: None,
            elapsed: 0.0,
            duration: 1.0,
            focus: None,
        }
    }
    fn start(&mut self, to: V3, look: V3, duration: f32) {
        self.path = Some(CameraPath::new(self.current, self.current_look, to, look, duration));
        self.elapsed = 0.0;
        self.duration = duration;
    }
}

#[derive(Resource, Default)]
struct Subtitle {
    full: String,
    shown: usize,
    acc: f32,
}

impl Subtitle {
    fn set(&mut self, speaker: &str, text: &str) {
        self.full = format!("{speaker}：{text}");
        self.shown = 0;
        self.acc = 0.0;
    }
}

/// 聊天/交互状态。
#[derive(Resource, Default)]
struct UiState {
    input: String,
    focused: bool,
    history: Vec<(bool, String)>,
    history_dirty: bool,
    speaking: f32,
}

#[derive(Component)]
struct MainCamera;

#[derive(Component)]
struct Avatar;

#[derive(Component)]
struct SubtitleText;

#[derive(Component)]
struct HistoryText;

#[derive(Component)]
struct InputBoxText;

/// 可点击元素的行为。
#[derive(Component, Clone)]
enum ClickAction {
    FocusInput,
    Send,
    Quick(String),
    Exhibit(usize),
}

#[derive(Component)]
struct StatusText;

// ───────────────────────────── 入口 ─────────────────────────────

fn main() {
    let asset_root = extract_embedded_assets().to_string_lossy().to_string();
    let theme = embedded_theme();
    let bg = hex_color(&theme.palette.bg);
    let key = hex_color(&theme.palette.key);
    let title = format!("{} · {} · 原生桌面版", theme.name, theme.assistant.name);
    let guide = make_guide(theme.clone());

    App::new()
        .add_plugins(
            DefaultPlugins
                .set(WindowPlugin {
                    primary_window: Some(Window {
                        title,
                        resolution: WindowResolution::new(1440, 900),
                        ..default()
                    }),
                    ..default()
                })
                .set(AssetPlugin {
                    file_path: asset_root,
                    unapproved_path_mode: UnapprovedPathMode::Allow,
                    ..default()
                }),
        )
        .insert_resource(ClearColor(bg))
        .insert_resource(GlobalAmbientLight {
            color: key,
            brightness: 220.0,
            ..default()
        })
        .insert_resource(Hall { theme, guide })
        .insert_resource(Cam::new())
        .insert_resource(Subtitle::default())
        .insert_resource(UiState::default())
        .add_systems(Startup, (setup_scene, setup_ui))
        .add_systems(
            Update,
            (
                text_input,
                ui_buttons,
                top_keys,
                camera_motion,
                update_subtitle,
                avatar_speak,
                sync_history,
            ),
        )
        .run();
}

fn hex_color(s: &str) -> Color {
    let s = s.trim_start_matches('#');
    let byte = |a: usize, b: usize| u8::from_str_radix(&s[a..b], 16).unwrap_or(0);
    match s.len() {
        6 => Color::srgb_u8(byte(0, 2), byte(2,4), byte(4,6)),
        3 => Color::srgb_u8(byte(0,1)*17, byte(1,2)*17, byte(2,3)*17),
        _ => Color::srgb(0.05, 0.04, 0.03),
    }
}

// ───────────────────────────── 场景 ─────────────────────────────

fn setup_scene(
    mut commands: Commands,
    asset_server: Res<AssetServer>,
    hall: Res<Hall>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    let p = &hall.theme.palette;
    let stone = materials.add(StandardMaterial { base_color: Color::srgb_u8(0xc0, 0xb3, 0xa0), perceptual_roughness: 0.56, ..default() });
    let stone_dark = materials.add(StandardMaterial { base_color: Color::srgb_u8(0xa7, 0x9a, 0x8c), perceptual_roughness: 0.66, ..default() });
    let brass = materials.add(StandardMaterial { base_color: Color::srgb_u8(0xc8, 0xa4, 0x5e), metallic: 0.95, perceptual_roughness: 0.32, ..default() });
    let floor_mat = materials.add(StandardMaterial { base_color: Color::srgb_u8(0x63, 0x59, 0x4f), metallic: 0.2, perceptual_roughness: 0.42, ..default() });
    let wall_mat = materials.add(StandardMaterial { base_color: Color::srgb_u8(0x6f, 0x5b, 0x45), perceptual_roughness: 0.95, ..default() });
    let ceil_mat = materials.add(StandardMaterial { base_color: Color::srgb_u8(0x1d, 0x18, 0x13), perceptual_roughness: 0.95, ..default() });

    // 地面：用薄长方体代替 Plane3d，避免 0.20 平面法线/尺寸的构造差异。
    commands.spawn((Mesh3d(meshes.add(Cuboid::new(60.0, 0.04, 60.0))), MeshMaterial3d(floor_mat.clone()), Transform::from_xyz(0.0, -0.02, -0.4)));
    commands.spawn((Mesh3d(meshes.add(Cylinder::new(2.12, 0.004))), MeshMaterial3d(brass.clone()), Transform::from_xyz(0.0, 0.006, 1.15)));
    commands.spawn((Mesh3d(meshes.add(Cylinder::new(2.05, 0.005))), MeshMaterial3d(floor_mat.clone()), Transform::from_xyz(0.0, 0.009, 1.15)));

    commands.spawn((Mesh3d(meshes.add(Cuboid::new(34.0, 9.2, 0.3))), MeshMaterial3d(wall_mat.clone()), Transform::from_xyz(0.0, 4.6, -7.4)));
    for side in [-1.0_f32, 1.0] {
        commands.spawn((Mesh3d(meshes.add(Cuboid::new(0.3, 9.2, 15.0))), MeshMaterial3d(wall_mat.clone()), Transform::from_xyz(side * 9.2, 4.6, -0.4)));
    }
    commands.spawn((Mesh3d(meshes.add(Cuboid::new(34.0, 0.3, 16.0))), MeshMaterial3d(ceil_mat), Transform::from_xyz(0.0, 8.55, -0.4)));

    for (i, ex) in hall.theme.exhibits.iter().enumerate() {
        let (x, z) = (ex.pos[0] as f32, ex.pos[2] as f32);
        let top_r = 0.46_f32;
        let body_r = top_r - 0.10;
        let discs = [
            (top_r + 0.04, 0.10, 0.05, stone_dark.clone()),
            (body_r, 0.55, 0.375, stone.clone()),
            (body_r + 0.012, 0.018, 0.645, brass.clone()),
            (top_r, 0.06, 0.68, stone.clone()),
            (top_r - 0.07, 0.012, 0.716, stone_dark.clone()),
        ];
        for (r, h, y, mat) in discs {
            commands.spawn((Mesh3d(meshes.add(Cylinder::new(r, h))), MeshMaterial3d(mat), Transform::from_xyz(x, y, z)));
        }

        if let Some(model) = &ex.model {
            commands.spawn((WorldAssetRoot(asset_server.load(GltfAssetLabel::Scene(0).from_asset(model.clone()))), Transform::from_xyz(x, 0.73, z).with_scale(Vec3::splat(0.7))));
        } else {
            let placeholder = materials.add(StandardMaterial { base_color: hex_color(&p.accent), perceptual_roughness: 0.6, ..default() });
            commands.spawn((Mesh3d(meshes.add(Sphere::new(0.22))), MeshMaterial3d(placeholder), Transform::from_xyz(x, 0.95, z)));
        }

        commands.spawn((PointLight { color: hex_color(&p.key), intensity: 1600.0, range: 8.0, ..default() }, Transform::from_xyz(x * 0.9 + 0.25, 3.6, z + 1.65)));
    }

    // 数字人：avatar.glb（VRM 基础网格）。表情/口型为原生近似，见说明。
    commands.spawn((WorldAssetRoot(asset_server.load(GltfAssetLabel::Scene(0).from_asset("models/avatar.glb"))), Transform::from_xyz(0.0, 0.0, 1.15), Avatar));

    commands.spawn((DirectionalLight { color: hex_color(&p.key), illuminance: 2600.0, shadow_maps_enabled: true, ..default() }, Transform::from_xyz(-2.2, 5.0, 3.6).looking_at(Vec3::new(0.0, 1.0, -1.0), Vec3::Y)));
    commands.spawn((PointLight { color: hex_color(&p.key), intensity: 2400.0, range: 12.0, ..default() }, Transform::from_xyz(1.6, 3.1, 3.6)));

    commands.spawn((Camera3d::default(), Projection::Perspective(PerspectiveProjection { fov: 34.0_f32.to_radians(), ..default() }), Transform::from_xyz(0.0, 1.5, 6.0).looking_at(Vec3::new(0.0, 1.0, -1.2), Vec3::Y), MainCamera));
}

// ───────────────────────────── 聊天 UI ─────────────────────────────

fn panel_bg() -> BackgroundColor {
    BackgroundColor(Color::srgb(0.06, 0.05, 0.03).with_alpha(0.86))
}
fn accent_bg() -> BackgroundColor {
    BackgroundColor(Color::srgb(0.78, 0.64, 0.37).with_alpha(0.9))
}
fn accent_dark_bg() -> BackgroundColor {
    BackgroundColor(Color::srgb(0.20, 0.16, 0.11).with_alpha(0.95))
}
fn input_bg() -> BackgroundColor {
    BackgroundColor(Color::srgb(0.02, 0.02, 0.02).with_alpha(0.95))
}

fn setup_ui(mut commands: Commands, asset_server: Res<AssetServer>, hall: Res<Hall>) {
    let font: FontSource = asset_server.load("fonts/simhei.ttf").into();
    let small = TextFont { font: font.clone(), font_size: FontSize::Px(15.0), ..default() };
    let big = TextFont { font: font.clone(), font_size: FontSize::Px(26.0), ..default() };
    let warm = Color::srgb_u8(0xe8, 0xd2, 0xa8);
    let dim = Color::srgb_u8(0x9c, 0x8d, 0x74);
    let white = Color::srgb_u8(0xff, 0xf3, 0xdc);

    // 顶栏
    commands.spawn((Node { position_type: PositionType::Absolute, left: Val::Px(0.0), top: Val::Px(0.0), width: Val::Percent(100.0), height: Val::Px(50.0), flex_direction: FlexDirection::Row, align_items: AlignItems::Center, justify_content: JustifyContent::SpaceBetween, padding: UiRect::new(Val::Px(14.0), Val::Px(14.0), Val::Px(10.0), Val::Px(10.0)), ..default() }, panel_bg(), ZIndex(10)))
        .with_children(|p| {
            p.spawn((Text::new(format!("{} · {} · 原生桌面版", hall.theme.name, hall.theme.assistant.name)), TextFont { font: font.clone(), font_size: FontSize::Px(20.0), ..default() }, TextColor(warm)));
            p.spawn((Text::new(format!("6 件展品 · {} 条答案 · 本地离线", hall.theme.kb_answers())), TextFont { font: font.clone(), font_size: FontSize::Px(13.0), ..default() }, TextColor(dim)));
        });

    // 左侧展项目录
    commands.spawn((Node { position_type: PositionType::Absolute, left: Val::Px(10.0), top: Val::Px(60.0), width: Val::Px(220.0), height: Val::Px(430.0), flex_direction: FlexDirection::Column, align_items: AlignItems::Start, justify_content: JustifyContent::Start, row_gap: Val::Px(8.0), padding: UiRect::all(Val::Px(12.0)), ..default() }, panel_bg(), ZIndex(10)))
        .with_children(|p| {
            p.spawn((Text::new("展项目录 · 点击讲解"), small.clone(), TextColor(warm)));
            for (i, ex) in hall.theme.exhibits.iter().enumerate() {
                p.spawn((Node { width: Val::Px(196.0), height: Val::Px(40.0), align_items: AlignItems::Center, justify_content: JustifyContent::Center, border_radius: BorderRadius::all(Val::Px(8.0)), ..default() }, accent_dark_bg(), Interaction::None, ClickAction::Exhibit(i)))
                    .with_children(|b| { b.spawn((Text::new(&ex.name), small.clone(), TextColor(white))); });
            }
        });

    // 右侧聊天面板：问答记录 + 快捷提问 + 输入框 + 发送
    commands.spawn((Node { position_type: PositionType::Absolute, right: Val::Px(10.0), top: Val::Px(60.0), width: Val::Px(372.0), height: Val::Px(700.0), flex_direction: FlexDirection::Column, align_items: AlignItems::Start, justify_content: JustifyContent::Start, row_gap: Val::Px(10.0), padding: UiRect::all(Val::Px(12.0)), ..default() }, panel_bg(), ZIndex(10)))
        .with_children(|p| {
            p.spawn((Node { width: Val::Px(348.0), height: Val::Px(470.0), overflow: Overflow::hidden(), ..default() }, BackgroundColor(Color::srgb(0.03, 0.025, 0.02).with_alpha(0.6)), HistoryText, Text::new(""), TextFont { font: font.clone(), font_size: FontSize::Px(15.0), ..default() }, TextColor(warm)));
            // 快捷提问
            p.spawn((Node { width: Val::Px(348.0), flex_direction: FlexDirection::Row, align_items: AlignItems::Center, justify_content: JustifyContent::Start, column_gap: Val::Px(8.0), ..default() }))
                .with_children(|r| {
                    for q in ["介绍一下碉楼", "什么是侨批", "参观路线"] {
                        r.spawn((Node { width: Val::Px(108.0), height: Val::Px(34.0), align_items: AlignItems::Center, justify_content: JustifyContent::Center, border_radius: BorderRadius::all(Val::Px(8.0)), ..default() }, accent_dark_bg(), Interaction::None, ClickAction::Quick(q.to_string())))
                            .with_children(|b| { b.spawn((Text::new(q), TextFont { font: font.clone(), font_size: FontSize::Px(12.0), ..default() }, TextColor(white))); });
                    }
                });
            // 输入框 + 发送
            p.spawn((Node { width: Val::Px(348.0), flex_direction: FlexDirection::Row, align_items: AlignItems::Center, justify_content: JustifyContent::Start, column_gap: Val::Px(8.0), ..default() }))
                .with_children(|r| {
                    r.spawn((Node { width: Val::Px(262.0), height: Val::Px(38.0), align_items: AlignItems::Center, padding: UiRect::all(Val::Px(8.0)), border_radius: BorderRadius::all(Val::Px(8.0)), ..default() }, input_bg(), Interaction::None, ClickAction::FocusInput, InputBoxText, Text::new(""), TextFont { font: font.clone(), font_size: FontSize::Px(15.0), ..default() }, TextColor(white)));
                    r.spawn((Node { width: Val::Px(78.0), height: Val::Px(38.0), align_items: AlignItems::Center, justify_content: JustifyContent::Center, border_radius: BorderRadius::all(Val::Px(8.0)), ..default() }, accent_bg(), Interaction::None, ClickAction::Send))
                        .with_children(|b| { b.spawn((Text::new("发送"), TextFont { font: font.clone(), font_size: FontSize::Px(15.0), ..default() }, TextColor(Color::srgb_u8(0x1a, 0x12, 0x08)))); });
                });
        });

    // 底部字幕（当前讲解）
    commands.spawn((Node { position_type: PositionType::Absolute, left: Val::Px(244.0), bottom: Val::Px(24.0), width: Val::Px(1040.0), ..default() }, Text::new(format!("{}：{}", hall.theme.assistant.name, hall.theme.welcome)), big.clone(), TextColor(Color::srgb_u8(0xff, 0xf3, 0xdc)), TextBackgroundColor(Color::srgb(0.03, 0.02, 0.01).with_alpha(0.82)), SubtitleText, ZIndex(10)));

    // 状态/提示
    commands.spawn((Node { position_type: PositionType::Absolute, left: Val::Px(10.0), bottom: Val::Px(14.0), ..default() }, Text::new("1–6 聚焦展品 · 空格 讲解 · Q 参观路线 · R 复位镜头 · Esc 退出"), TextFont { font: font.clone(), font_size: FontSize::Px(12.0), ..default() }, TextColor(dim), StatusText, ZIndex(10)));
}

// ───────────────────────────── 交互与动画 ─────────────────────────────

fn perform_ask(hall: &Hall, cam: &mut Cam, ui: &mut UiState, sub: &mut Subtitle, question: &str) {
    let q = question.trim();
    if q.is_empty() {
        return;
    }
    let answer = hall.guide.answer(q, None, false, None);
    sub.set(&hall.theme.assistant.name, &answer.reply);
    ui.history.push((true, q.to_string()));
    ui.history.push((false, answer.reply.clone()));
    ui.history_dirty = true;
    if let Some(id) = &answer.exhibit {
        if let Some(i) = hall.theme.exhibits.iter().position(|e| &e.id == id) {
            let ex = &hall.theme.exhibits[i];
            let pos = V3::new(ex.pos[0] as f32, 1.36, ex.pos[2] as f32);
            let (to, look) = focus_view(cam.base, pos, 0.55);
            cam.start(to, look, 0.85);
            cam.focus = Some(i);
        }
    }
    let dur = (answer.reply.chars().count() as f64 * 0.18).clamp(1.2, 18.0);
    ui.speaking = dur as f32;
    speak(&answer.reply);
    ui.input.clear();
}

/// Windows 系统语音（System.Speech），非阻塞；不可用时静默跳过。
fn speak(text: &str) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let t = text.replace('\'', "''");
        let script = format!(
            "Add-Type -AssemblyName System.Speech; $s=New-Object System.Speech.Synthesis.SpeechSynthesizer; $s.SetOutputToDefaultAudioDevice(); $s.Speak('{}'); $s.Dispose();",
            t
        );
        let _ = std::process::Command::new("powershell")
            .args(["-NoProfile", "-NonInteractive", "-Command", &script])
            .creation_flags(0x0800_0000)
            .spawn();
    }
    let _ = text;
}

fn text_input(
    mut chars: MessageReader<KeyboardInput>,
    hall: Res<Hall>,
    mut cam: ResMut<Cam>,
    mut ui: ResMut<UiState>,
    mut sub: ResMut<Subtitle>,
    mut q_input: Query<&mut Text, With<InputBoxText>>,
) {
    if ui.focused {
        for e in chars.read() {
            if e.state != ButtonState::Pressed || e.repeat {
                continue;
            }
            match e.key_code {
                KeyCode::Backspace => {
                    ui.input.pop();
                }
                KeyCode::Enter => {
                    let q = ui.input.clone();
                    perform_ask(&hall, &mut cam, &mut ui, &mut sub, &q);
                }
                _ => {
                    if let Some(t) = &e.text {
                        for c in t.chars() {
                            if !c.is_control() && ui.input.chars().count() < 200 {
                                ui.input.push(c);
                            }
                        }
                    }
                }
            }
        }
    }
    for mut t in &mut q_input {
        let caret = if ui.focused { "▌" } else { "" };
        t.0 = format!("{}{}", ui.input, caret);
    }
}

fn ui_buttons(
    mut interactions: Query<(Entity, &Interaction, &ClickAction)>,
    mut pressed: Local<HashSet<Entity>>,
    hall: Res<Hall>,
    mut cam: ResMut<Cam>,
    mut ui: ResMut<UiState>,
    mut sub: ResMut<Subtitle>,
) {
    for (e, inter, action) in &mut interactions {
        let is_pressed = *inter == Interaction::Pressed;
        let was_pressed = pressed.contains(&e);
        match action {
            ClickAction::FocusInput => {
                if is_pressed && !was_pressed {
                    ui.focused = true;
                }
            }
            _ => {
                if is_pressed && !was_pressed {
                    ui.focused = false;
                    match action {
                        ClickAction::Send => {
                            let q = ui.input.clone();
                            perform_ask(&hall, &mut cam, &mut ui, &mut sub, &q);
                        }
                        ClickAction::Quick(q) => {
                            perform_ask(&hall, &mut cam, &mut ui, &mut sub, q);
                        }
                        ClickAction::Exhibit(i) => {
                            if let Some(ex) = hall.theme.exhibits.get(*i) {
                                let q = format!("介绍{}", ex.name);
                                perform_ask(&hall, &mut cam, &mut ui, &mut sub, &q);
                            }
                        }
                        ClickAction::FocusInput => {}
                    }
                }
            }
        }
        if is_pressed {
            pressed.insert(e);
        } else {
            pressed.remove(&e);
        }
    }
}

fn top_keys(
    keys: Res<ButtonInput<KeyCode>>,
    hall: Res<Hall>,
    mut cam: ResMut<Cam>,
    mut sub: ResMut<Subtitle>,
    ui: Res<UiState>,
) {
    if ui.focused {
        return;
    }
    if keys.just_pressed(KeyCode::Escape) {
        std::process::exit(0);
    }
    let digits = [KeyCode::Digit1, KeyCode::Digit2, KeyCode::Digit3, KeyCode::Digit4, KeyCode::Digit5, KeyCode::Digit6];
    for (i, key) in digits.iter().enumerate() {
        if !keys.just_pressed(*key) || i >= hall.theme.exhibits.len() {
            continue;
        }
        let ex = &hall.theme.exhibits[i];
        let pos = V3::new(ex.pos[0] as f32, 1.36, ex.pos[2] as f32);
        let (to, look) = focus_view(cam.base, pos, 0.55);
        cam.start(to, look, 0.85);
        cam.focus = Some(i);
        sub.set(&hall.theme.assistant.name, &format!("正在看「{}」，按空格听我讲解。", ex.name));
    }
    if keys.just_pressed(KeyCode::Space) {
        let name = hall.theme.assistant.name.clone();
        let question = match cam.focus {
            Some(i) => format!("介绍{}", hall.theme.exhibits[i].name),
            None => hall.theme.welcome.clone(),
        };
        let focus_id = cam.focus.map(|i| hall.theme.exhibits[i].id.as_str());
        let answer = hall.guide.answer(&question, focus_id, false, None);
        sub.set(&name, &answer.reply);
        if let Some(id) = &answer.exhibit {
            if let Some(i) = hall.theme.exhibits.iter().position(|e| &e.id == id) {
                let ex = &hall.theme.exhibits[i];
                let pos = V3::new(ex.pos[0] as f32, 1.36, ex.pos[2] as f32);
                let (to, look) = focus_view(cam.base, pos, 0.55);
                cam.start(to, look, 0.85);
                cam.focus = Some(i);
            }
        }
    }
    if keys.just_pressed(KeyCode::KeyQ) {
        let answer = hall.guide.answer("参观路线", None, false, None);
        sub.set(&hall.theme.assistant.name, &answer.reply);
    }
    if keys.just_pressed(KeyCode::KeyR) {
        let (base, base_look) = (cam.base, cam.base_look);
        cam.start(base, base_look, 1.0);
        cam.focus = None;
        sub.set(&hall.theme.assistant.name, "镜头已回到全景。");
    }
}

fn camera_motion(time: Res<Time>, mut cam: ResMut<Cam>, mut query: Query<&mut Transform, With<MainCamera>>) {
    let Some(path) = cam.path.clone() else { return };
    cam.elapsed += time.delta_secs();
    let t = (cam.elapsed / cam.duration).min(1.0);
    let (pos, look) = path.sample(t);
    cam.current = pos;
    cam.current_look = look;
    let target = Vec3::new(look.x, look.y, look.z);
    let translation = Vec3::new(pos.x, pos.y, pos.z);
    let rotation = Transform::from_translation(translation).looking_at(target, Vec3::Y).rotation;
    for mut transform in &mut query {
        transform.translation = translation;
        transform.rotation = rotation;
    }
    if t >= 1.0 {
        cam.path = None;
    }
}

fn update_subtitle(time: Res<Time>, mut sub: ResMut<Subtitle>, mut query: Query<&mut Text, With<SubtitleText>>) {
    let chars: Vec<char> = sub.full.chars().collect();
    if sub.shown >= chars.len() {
        return;
    }
    sub.acc += time.delta_secs();
    const STEP: f32 = 0.045;
    while sub.acc >= STEP && sub.shown < chars.len() {
        sub.acc -= STEP;
        sub.shown += 1;
    }
    let shown: String = chars[..sub.shown].iter().collect();
    for mut text in &mut query {
        text.0 = shown.clone();
    }
}

fn avatar_speak(time: Res<Time>, mut ui: ResMut<UiState>, mut query: Query<&mut Transform, With<Avatar>>) {
    let speaking = ui.speaking > 0.0;
    if speaking {
        ui.speaking = (ui.speaking - time.delta_secs()).max(0.0);
    }
    for mut tr in &mut query {
        tr.translation.y = if speaking { (time.elapsed_secs() * 9.0).sin() * 0.02 } else { 0.0 };
    }
}

fn sync_history(mut ui: ResMut<UiState>, mut q: Query<&mut Text, With<HistoryText>>) {
    if !ui.history_dirty {
        return;
    }
    ui.history_dirty = false;
    let mut s = String::new();
    let start = ui.history.len().saturating_sub(16);
    for (is_user, msg) in &ui.history[start..] {
        s.push_str(if *is_user { "你：" } else { "小侨：" });
        s.push_str(msg);
        s.push_str("\n\n");
    }
    for mut t in &mut q {
        t.0 = s.clone();
    }
}
