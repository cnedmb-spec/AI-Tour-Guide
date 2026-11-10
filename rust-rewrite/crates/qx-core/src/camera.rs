//! 相机路径：把 `app/src/scene.js` 里修好的绕行算法移植为渲染无关的纯函数。
//!
//! 2026-11-08 的演示录屏在 16.9–17.6s 出现相机穿过数字人身体，根因是特写机位
//! 落在数字人所在的 z 平面、复位又走直线插值。修复版做了两件事：
//!   1. 特写机位横向让开中轴、并钳制 z 不越过人物平面；
//!   2. 直线若穿过禁入区，改走带侧向控制点的二次贝塞尔绕行。
//! 这里用 Liang–Barsky 解析求交判定「是否穿过」，避免固定步数采样漏掉擦边路径。
//! 本模块只依赖 std，可被 Bevy、gpui-kit 或任何渲染层复用，且能穷举验证。

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct V3 {
    pub x: f32,
    pub y: f32,
    pub z: f32,
}

impl V3 {
    pub const fn new(x: f32, y: f32, z: f32) -> Self {
        Self { x, y, z }
    }
    pub fn lerp(self, other: Self, t: f32) -> Self {
        Self::new(
            self.x + (other.x - self.x) * t,
            self.y + (other.y - self.y) * t,
            self.z + (other.z - self.z) * t,
        )
    }
    /// 与渲染层的 Vec3 互转由调用方完成，核心层不依赖任何数学库。
    pub fn as_tuple(self) -> (f32, f32, f32) {
        (self.x, self.y, self.z)
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Keepout {
    pub min_x: f32,
    pub max_x: f32,
    pub min_z: f32,
    pub max_z: f32,
}

/// 数字人站在场景中央（x≈0、z≈1.15）所占的 xz 投影区域，与 scene.js 一致。
pub const KEEPOUT: Keepout = Keepout {
    min_x: -1.15,
    max_x: 1.15,
    min_z: 0.25,
    max_z: 2.1,
};

pub fn in_keepout(x: f32, z: f32) -> bool {
    x > KEEPOUT.min_x && x < KEEPOUT.max_x && z > KEEPOUT.min_z && z < KEEPOUT.max_z
}

/// Liang–Barsky 线段-矩形求交（xz 投影）。解析判定，无采样漏检。
pub fn segment_hits_keepout(a: V3, b: V3) -> bool {
    let (dx, dz) = (b.x - a.x, b.z - a.z);
    let p = [-dx, dx, -dz, dz];
    let q = [
        a.x - KEEPOUT.min_x,
        KEEPOUT.max_x - a.x,
        a.z - KEEPOUT.min_z,
        KEEPOUT.max_z - a.z,
    ];
    let (mut t0, mut t1) = (0.0_f32, 1.0_f32);
    for i in 0..4 {
        if p[i] == 0.0 {
            if q[i] < 0.0 {
                return false; // 平行于该边且整段落在矩形外
            }
        } else {
            let r = q[i] / p[i];
            if p[i] < 0.0 {
                if r > t1 {
                    return false;
                }
                t0 = t0.max(r);
            } else {
                if r < t0 {
                    return false;
                }
                t1 = t1.min(r);
            }
        }
    }
    t0 <= t1
}

fn clamp(v: f32, lo: f32, hi: f32) -> f32 {
    v.max(lo).min(hi)
}

/// 展品特写机位。与 scene.js focusOn() 一致：按展品高度推距离，再横向让开中轴、
/// 且不允许越过数字人所在的 z 平面。返回 (机位, 视点)。
pub fn focus_view(base: V3, exhibit: V3, exhibit_h: f32) -> (V3, V3) {
    let d = clamp(exhibit_h * 2.05, 1.8, 4.8);
    let (dx, dz) = (base.x - exhibit.x, base.z - exhibit.z);
    let len = (dx * dx + dz * dz).sqrt();
    let (ux, uz) = if len > 1e-6 { (dx / len, dz / len) } else { (0.0, 1.0) };
    let side = if exhibit.x >= 0.0 { 1.0 } else { -1.0 };
    let look_y = 0.73 + exhibit_h * 0.5;
    let px = side * 1.6_f32.max((exhibit.x + ux * d).abs());
    let pz = 0.15_f32.min(exhibit.z + uz * d);
    (
        V3::new(px, look_y + 0.18, pz),
        V3::new(exhibit.x, look_y, exhibit.z),
    )
}

/// 修复前的特写机位（原版直线推近），仅用于回归测试中复现缺陷。
pub fn legacy_focus_view(base: V3, exhibit: V3, exhibit_h: f32) -> (V3, V3) {
    let d = clamp(exhibit_h * 2.05, 1.8, 4.8);
    let (dx, dz) = (base.x - exhibit.x, base.z - exhibit.z);
    let len = (dx * dx + dz * dz).sqrt();
    let (ux, uz) = if len > 1e-6 { (dx / len, dz / len) } else { (0.0, 1.0) };
    let look_y = 0.73 + exhibit_h * 0.5;
    (
        V3::new(exhibit.x + ux * d, look_y + 0.18, exhibit.z + uz * d),
        V3::new(exhibit.x, look_y, exhibit.z),
    )
}

pub fn ease_in_out_cubic(t: f32) -> f32 {
    if t < 0.5 {
        4.0 * t * t * t
    } else {
        1.0 - (-2.0 * t + 2.0).powi(3) / 2.0
    }
}

/// 一次完整的镜头移动：固定时长缓动 + 视点同步插值 + 必要时侧向绕行。
#[derive(Debug, Clone)]
pub struct CameraPath {
    pub from: V3,
    pub from_look: V3,
    pub to: V3,
    pub to_look: V3,
    pub ctrl: Option<V3>,
    pub duration: f32,
}

impl CameraPath {
    pub fn new(from: V3, from_look: V3, to: V3, to_look: V3, duration: f32) -> Self {
        let ctrl = if segment_hits_keepout(from, to) {
            let side = if from.x + to.x >= 0.0 { 1.0 } else { -1.0 };
            Some(V3::new(
                side * 2.4_f32.max(from.x.abs() + 1.4).max(to.x.abs() + 1.4),
                from.y.max(to.y) + 0.3,
                (from.z + to.z) * 0.5,
            ))
        } else {
            None
        };
        Self { from, from_look, to, to_look, ctrl, duration: duration.max(0.001) }
    }

    pub fn detour(&self) -> bool {
        self.ctrl.is_some()
    }

    /// 归一化进度 0..1 → (机位, 视点)。
    pub fn sample(&self, t01: f32) -> (V3, V3) {
        let t = ease_in_out_cubic(t01.clamp(0.0, 1.0));
        let pos = match self.ctrl {
            Some(c) => {
                let u = 1.0 - t;
                V3::new(
                    u * u * self.from.x + 2.0 * u * t * c.x + t * t * self.to.x,
                    u * u * self.from.y + 2.0 * u * t * c.y + t * t * self.to.y,
                    u * u * self.from.z + 2.0 * u * t * c.z + t * t * self.to.z,
                )
            }
            None => self.from.lerp(self.to, t),
        };
        (pos, self.from_look.lerp(self.to_look, t))
    }

    /// 供测试穷举：路径上等距采样是否进入禁入区。
    pub fn hits_keepout(&self, samples: usize) -> bool {
        (0..=samples).any(|i| {
            let (p, _) = self.sample(i as f32 / samples as f32);
            in_keepout(p.x, p.z)
        })
    }

    /// 对照用：假设直接用直线插值（修复前的行为）。
    pub fn straight_hits_keepout(&self, samples: usize) -> bool {
        (0..=samples).any(|i| {
            let p = self.from.lerp(self.to, i as f32 / samples as f32);
            in_keepout(p.x, p.z)
        })
    }
}
