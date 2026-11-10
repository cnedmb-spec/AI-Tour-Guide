//! 相机绕行算法的确定性回归：与 11-08 的 Python 网格验证同口径。

use qx_core::camera::{
    focus_view, in_keepout, legacy_focus_view, segment_hits_keepout, CameraPath, V3, KEEPOUT,
};

/// theme.json 里 6 个展品的 xz 坐标（y 均为 0）。
const EXHIBITS: &[(&str, f32, f32)] = &[
    ("diaolou", 1.1, -3.9),
    ("chikan", 2.2, -3.4),
    ("liang", -3.3, -2.9),
    ("chenpi", -1.1, -3.8),
    ("qiaopi", -2.2, -3.5),
    ("bird", 3.3, -2.9),
];

#[test]
fn keepout_matches_js_constants() {
    assert_eq!(
        (KEEPOUT.min_x, KEEPOUT.max_x, KEEPOUT.min_z, KEEPOUT.max_z),
        (-1.15, 1.15, 0.25, 2.1)
    );
    assert!(in_keepout(0.0, 1.15));
    assert!(!in_keepout(2.0, 1.15));
    assert!(!in_keepout(0.0, -3.0));
}

#[test]
fn analytic_detector_catches_grazing_paths_that_32_sample_misses() {
    // 从真实取景网格里找出「解析判定相交、但 32 段采样全落在矩形外」的擦边路径。
    // 这类假阴性正是 11-08 用采样法做绕行判定时的隐患，改用 Liang–Barsky 后必须被捕获。
    let heights = [0.4_f32, 0.55, 0.8, 1.2, 1.8, 2.4];
    let base_zs = [4.0_f32, 4.5, 5.0, 5.5, 6.0, 6.5];
    let mut analytic_hits = 0;
    let mut sampling_false_negatives = 0;
    for &(_, x, z) in EXHIBITS {
        for &h in &heights {
            for &bz in &base_zs {
                let base = V3::new(0.0, 1.5, bz);
                let (focus, _) = focus_view(base, V3::new(x, 1.36, z), h);
                for (a, b) in [(base, focus), (focus, base)] {
                    if !segment_hits_keepout(a, b) {
                        continue;
                    }
                    analytic_hits += 1;
                    let sampled = (0..=32).any(|i| {
                        let p = a.lerp(b, i as f32 / 32.0);
                        in_keepout(p.x, p.z)
                    });
                    if !sampled {
                        sampling_false_negatives += 1;
                    }
                }
            }
        }
    }
    assert!(analytic_hits > 0, "网格中应存在需要绕行的路径");
    assert!(sampling_false_negatives > 0, "应复现 32 段采样漏检的擦边路径");

    // 明确不相交的情形不能被误判
    assert!(!segment_hits_keepout(V3::new(-3.0, 1.5, 3.0), V3::new(3.0, 1.5, 3.0)));
    assert!(!segment_hits_keepout(V3::new(-3.0, 1.5, -4.0), V3::new(3.0, 1.5, -4.0)));
}

#[test]
fn all_focus_paths_avoid_the_avatar_on_a_full_grid() {
    let heights = [0.4_f32, 0.55, 0.8, 1.2, 1.8, 2.4];
    let base_zs = [4.0_f32, 4.5, 5.0, 5.5, 6.0, 6.5];
    let mut checked = 0;
    let mut legacy_hits = 0;
    for &(_, x, z) in EXHIBITS {
        for &h in &heights {
            for &bz in &base_zs {
                let base = V3::new(0.0, 1.5, bz);
                let base_look = V3::new(0.0, 1.0, -1.2);
                let exhibit = V3::new(x, 1.36, z);
                let (focus, look) = focus_view(base, exhibit, h);

                assert!(!in_keepout(focus.x, focus.z), "特写机位落入禁入区: {x}, {h}, {bz}");

                let push = CameraPath::new(base, base_look, focus, look, 0.85);
                let pull = CameraPath::new(focus, look, base, base_look, 1.0);
                assert!(!push.hits_keepout(400), "推进路径穿模: {x}, {h}, {bz}");
                assert!(!pull.hits_keepout(400), "复位路径穿模: {x}, {h}, {bz}");
                checked += 2;

                // 对照组：修复前的直线插值 + 原版特写机位，应能复现缺陷
                let (old_focus, old_look) = legacy_focus_view(base, exhibit, h);
                let old_pull = CameraPath::new(old_focus, old_look, base, base_look, 1.0);
                if old_pull.straight_hits_keepout(400) {
                    legacy_hits += 1;
                }
            }
        }
    }
    assert_eq!(checked, 432, "6 展品 × 6 高度 × 6 基准机位 × 2 方向");
    assert!(legacy_hits > 0, "对照组未能复现原缺陷，网格失去意义");
}

#[test]
fn camera_path_is_deterministic_and_has_fixed_endpoints() {
    let from = V3::new(0.0, 1.5, 6.0);
    let from_look = V3::new(0.0, 1.0, -1.2);
    let to = V3::new(1.6, 1.2, 0.1);
    let to_look = V3::new(1.1, 1.02, -3.9);
    let p = CameraPath::new(from, from_look, to, to_look, 0.85);
    assert_eq!(p.sample(0.0).0, from);
    assert_eq!(p.sample(0.0).1, from_look);
    assert_eq!(p.sample(1.0).0, to);
    assert_eq!(p.sample(1.0).1, to_look);
    assert_eq!(p.sample(0.5), p.sample(0.5), "纯函数，重复采样必须一致");
    assert!(p.detour(), "该路径穿过禁入区，应启用绕行");
    assert!(!p.hits_keepout(2000));
}
