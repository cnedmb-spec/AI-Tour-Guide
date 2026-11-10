// 讲解手势编排：纯函数，输入手势相位 / 展品方位 / 语音能量，输出双臂各骨骼通道目标值（rad）。
// 不依赖 three.js 或 DOM，便于单独做确定性回归（.diag/gesture_test.mjs）。
//
// ── 为什么重写（060750 录屏反馈「动作生硬、不真实」）──
// 上一版是「离散姿势 + 段末过渡」：一个姿势摆好后定住 1~1.6 秒完全不动，
// 逐帧统计约 70% 的帧手部速度≈0，看起来就是提线木偶。且手往身体中线收
// （inward +0.2，手 x≈-0.12），像捂胸口而不是摊手讲解。
//
// 本版换成「连续手势笔画流」：
//   · 每个笔画是一次完整的 起势→抬手→顶点→回落，全程都在动，笔画之间不留空档；
//   · 抬手用带轻微过冲的缓出曲线、回落用缓入缓出，模拟肌肉惯性；
//   · 前臂比大臂晚 70ms、掌心比前臂再晚 60ms（关节 overlap），不会整条手臂同时到位；
//   · 叠加永不停止的三谐波微动，任何一帧都不完全静止；
//   · 幅度与语音 RMS 包络耦合，重音时抬得更高。
//
// 通道语义（依据 .diag/axis_scan.html + pose_scan2/3/4.html 共 64 组真实 3D 定量扫描）：
//   ux      upperArm.rotation.x  负值 = 大臂前摆抬手
//   z       upperArm.rotation.z  外展量，1.30 贴身、1.10 略展开
//   uy      upperArm.rotation.y  水平内外，基准 0.45
//   twist   lowerArm.rotation.x  前臂扭转 + 掌心朝向
//   efwd    lowerArm.rotation.y  前臂前送
//   inward  lowerArm.rotation.z  前臂内收；正值把手拉向中线（捂胸），负值送前并翻掌心向上
//
// 姿势坐标实测：静息右手 (-0.21, 0.87, -0.15)；摊手 (-0.18, 1.22, 0.38)，掌心朝上。

const REST = { ux: 0.18, z: 1.30, uy: 0.45, twist: 0.70, efwd: 0, inward: 0 };

// 关键姿势：pose_scan4 实拍筛出「手离开躯干、掌心朝上」的 inward 负值区。
// 不再用 +0.2 那种把手拉向中线的姿势。
const POSES = {
  offer:    { ux: -1.00, z: 1.28, uy: 0.45, twist: 0.55, efwd: 0.42, inward: -0.42 }, // 摊手介绍
  emphasis: { ux: -1.16, z: 1.24, uy: 0.48, twist: 0.30, efwd: 0.46, inward: -0.32 }, // 抬手强调
  light:    { ux: -0.78, z: 1.28, uy: 0.45, twist: 0.75, efwd: 0.34, inward: -0.28 }, // 随语句轻抬
  point:    { ux: -1.05, z: 1.18, uy: 0.45, twist: 0.20, efwd: 0.52, inward: -0.20 }, // 指向展品
  spread:   { ux: -0.92, z: 1.14, uy: 0.45, twist: 0.60, efwd: 0.44, inward: -0.34 }, // 双手展开比大小
  beat:     { ux: -0.66, z: 1.30, uy: 0.45, twist: 0.68, efwd: 0.26, inward: -0.20 }, // 轻节拍（幅度要看得见）
};

// [起始秒, 抬手秒, 顶点秒, 回落秒, 姿势, 主动作手, 强度]
// 刻意长短不一、单双手交替、穿插轻节拍；起止没有固定节拍，不像正弦循环。
const STROKES = [
  [0.02, 0.42, 0.16, 0.55, 'offer',    'r',    1.00],
  [1.18, 0.34, 0.10, 0.46, 'beat',     'l',    0.70],
  [1.95, 0.50, 0.20, 0.62, 'emphasis', 'r',    1.00],
  [3.14, 0.40, 0.14, 0.50, 'light',    'l',    0.78],
  [4.04, 0.46, 0.20, 0.58, 'spread',   'both', 0.95],
  [5.18, 0.34, 0.10, 0.44, 'beat',     'r',    0.65],
  [5.92, 0.48, 0.18, 0.60, 'offer',    'l',    1.00],
  [7.08, 0.38, 0.14, 0.52, 'point',    'r',    0.92],
  [8.02, 0.44, 0.16, 0.56, 'light',    'l',    0.72],
  [8.94, 0.52, 0.22, 0.64, 'emphasis', 'r',    1.00],
  [10.18, 0.36, 0.12, 0.46, 'beat',    'l',    0.62],
  [10.92, 0.48, 0.20, 0.60, 'spread',  'both', 0.92],
  [12.08, 0.40, 0.14, 0.52, 'offer',   'r',    0.95],
  [13.02, 0.34, 0.10, 0.44, 'beat',    'l',    0.60],
  [13.72, 0.50, 0.22, 0.62, 'point',   'l',    0.92],
  [14.94, 0.42, 0.16, 0.54, 'light',   'r',    0.76],
  [15.84, 0.48, 0.20, 0.58, 'emphasis','l',    1.00],
  [17.00, 0.36, 0.12, 0.46, 'beat',    'r',    0.62],
  [17.74, 0.46, 0.18, 0.56, 'offer',   'r',    0.95],
  [18.88, 0.40, 0.14, 0.52, 'spread',  'both', 0.88],
  [19.98, 0.50, 0.22, 0.62, 'emphasis','l',    1.00],
  [21.24, 0.34, 0.10, 0.44, 'beat',    'r',    0.60],
  [21.92, 0.44, 0.18, 0.56, 'light',   'l',    0.74],
  [22.88, 0.42, 0.16, 0.54, 'offer',   'r',    0.92],
];
const CYCLE = 24.4;   // 末尾留 0.4 秒换气；跨边界的收尾由 wrap 求值补上

const LAG_LOWER = 0.07;   // 前臂相对大臂的延迟（秒）
const LAG_PALM  = 0.13;   // 掌心/扭转相对大臂的延迟（秒）

// 讲解态基线偏移：真人讲解员不会每说完一句就把手完全垂回腿侧，而是让手
// 保持在腹前的「手势空间」里，从那里起势、回落到那里。pose_scan5 实拍确认
// ux -0.18 一档仍是自然放松站姿，不会出现端盘子或手臂扭曲。
// 平移整条轨迹而非改 POSES，所以各笔画的抬手幅度（delta）保持不变。
const READY_BIAS = { ux: -0.15, efwd: 0.07, inward: -0.05 };

function clamp(v, lo, hi) { return v < lo ? lo : (v > hi ? hi : v); }
function easeInOut(x) {
  const t = clamp(x, 0, 1);
  return t < 0.5 ? 4 * t * t * t : 1 - Math.pow(-2 * t + 2, 3) / 2;
}
// 起止导数都为 0 的平滑抬手曲线。不能用 easeOutBack：它在起点导数为 3.6，
// 手臂从静止瞬间进入高速，实测产生 0.17 rad/帧 的「猛地一抽」，正是生硬感来源。
function smootherstep(x) {
  const t = clamp(x, 0, 1);
  return t * t * t * (t * (t * 6 - 15) + 10);
}

// 单个笔画在相对时间 tau 处的包络 [0, ~1.02]。
// 惯性放在「到位之后」而不是起势：真人手臂到位会有极小的回弹，且顶点段仍在动。
function envelope(tau, rise, hold, fall, lag = 0) {
  // 收手略干脆（名义值的 95%）。试过加长到 118%，低速帧反而从 15.9% 涨到
  // 18.2%：拖长的尾巴只会让手「慢慢滑回去」，更像提线木偶。
  const f = fall * 0.95;
  const t = tau - lag;
  if (t <= 0 || t >= rise + hold + f) return 0;
  if (t < rise) return smootherstep(t / rise);
  if (t < rise + hold) {
    // 顶点段不是「定住」：真人到位后手还在小幅比划。
    // 用两端归零的窗函数包住摆动，保证与抬手/回落衔接处连续、不跳变。
    const h = t - rise;
    const window = Math.sin(Math.PI * h / hold);
    return 1 + window * 0.055 * Math.sin(h * 16);
  }
  return 1 - easeInOut((t - rise - hold) / f);
}

function addScaled(out, pose, w) {
  for (const k in REST) out[k] += (pose[k] - REST[k]) * w;
}

// 1) 笔画层：大臂 / 前臂 / 掌心各自用延迟过的包络求值
function strokeTargets(time, mirror) {
  const upper = { L: { ...REST }, R: { ...REST } };
  const lower = { L: { ...REST }, R: { ...REST } };
  const palm  = { L: { ...REST }, R: { ...REST } };
  let energy = 0;
  // 对 t 与 t-CYCLE 各求值一次，跨循环边界的笔画不会突然断掉
  for (const base of [time, time - CYCLE]) {
    for (const [t0, rise, hold, fall, poseName, mainHand, amp] of STROKES) {
      const tau = base - t0;
      if (tau <= -LAG_PALM || tau >= rise + hold + fall + 0.25) continue;
      const p = POSES[poseName];
      const w  = envelope(tau, rise, hold, fall, 0) * amp;
      const wl = envelope(tau, rise, hold, fall, LAG_LOWER) * amp;
      const wp = envelope(tau, rise, hold, fall, LAG_PALM) * amp;
      if (w <= 0 && wl <= 0 && wp <= 0) continue;
      energy = Math.max(energy, w);
      const hand = mirror ? (mainHand === 'r' ? 'l' : (mainHand === 'l' ? 'r' : 'both')) : mainHand;
      const sides = hand === 'both' ? ['L', 'R'] : [hand === 'r' ? 'R' : 'L'];
      for (const s of sides) {
        const scale = (hand === 'both' || s === (hand === 'r' ? 'R' : 'L')) ? 1 : 0;
        addScaled(upper[s], p, w * scale);
        addScaled(lower[s], p, wl * scale);
        addScaled(palm[s],  p, wp * scale);
      }
      // 副手不是「停住等轮到自己」：真人双手会配合着动，只是幅度小、节奏稍晚。
      // 060750 录屏里主手轮换时另一只手几乎定住 0.6 秒，就是 0.14 联动太小造成的。
      const offS = hand === 'both' ? null : (hand === 'r' ? 'L' : 'R');
      if (offS) {
        const OFF = 0.40, OFF_LAG = 0.10;
        addScaled(upper[offS], p, envelope(tau, rise, hold, fall, OFF_LAG) * amp * OFF);
        addScaled(lower[offS], p, envelope(tau, rise, hold, fall, LAG_LOWER + OFF_LAG) * amp * OFF);
        addScaled(palm[offS],  p, envelope(tau, rise, hold, fall, LAG_PALM + OFF_LAG) * amp * OFF);
      }
    }
  }
  return { upper, lower, palm, energy: clamp(energy, 0, 1.2) };
}

// 2) 持续微动层：三谐波叠加，任何一帧都不为零，消除「摆好就定住」。
// 幅度约 0.01 rad，手部位移 0.002~0.005 m/帧，看着是「活着」而不是抖动。
function micro(out, t, side, vis, mirror) {
  // 镜像时左右手的微动相位也要跟着换，否则 side 取反后两侧动作不对称。
  // 左右仍用不同相位，避免双手像镜像同步的机器人。
  const rSide = mirror ? (side === 'R' ? 'L' : 'R') : side;
  const ph = rSide === 'R' ? 0 : 1.9;
  const m = 0.014 + vis * 0.008;
  out.ux     += m * 1.2 * (Math.sin(t * 1.13 + ph) + 0.5 * Math.sin(t * 2.31 + ph * 1.7));
  out.z      += m * 0.8 * Math.sin(t * 0.57 + ph);
  out.uy     += m * 0.5 * Math.sin(t * 0.83 + ph * 2.1);
  out.efwd   += m * 0.6 * Math.sin(t * 1.41 + ph);
  out.inward += m * 0.5 * Math.sin(t * 0.97 + ph);
  out.twist  += m * 0.7 * Math.sin(t * 1.27 + ph);
  // 双手自然的慢速游走：约 0.5Hz、幅度 4°，是「人在说话时手不会定住」的主要来源。
  // 频率低所以不抖，但幅度足以让笔画交接处仍有可见位移。
  out.ux     += 0.070 * Math.sin(t * 3.1 + ph) + 0.035 * Math.sin(t * 1.3 + ph * 1.4);
  out.efwd   += 0.030 * Math.sin(t * 2.4 + ph * 0.8);
}

// 把三层合成最终通道：大臂用 upper，前臂位移用 lower，掌心扭转用 palm
function compose(upper, lower, palm) {
  return {
    ux: upper.ux, z: upper.z, uy: upper.uy,
    twist: palm.twist,
    efwd: lower.efwd,
    inward: lower.inward,
  };
}

/**
 * @param {string} style 展项风格：point / spread / hands（只影响幅度与偏好）
 * @param {number} side  展品在角色左右，归一化到 [-1, 1]
 * @param {number} gestureT 讲解开始后的累计秒数
 * @param {number} t     全局时间（微动相位用，保证切展项不清零）
 * @param {number} visAmp 当前语音能量，[0, 1]
 * @returns {{left: object, right: object, energy: number}}
 */
export function explainGestureTargets(style, side, gestureT, t, visAmp) {
  const s = Math.max(-1, Math.min(1, side));
  const a = clamp(visAmp, 0, 1);
  const mirror = s < 0;
  const gain = (style === 'spread' ? 1.06 : (style === 'point' ? 1.04 : 1)) * (0.88 + a * 0.24);
  const { upper, lower, palm, energy } = strokeTargets(((gestureT % CYCLE) + CYCLE) % CYCLE, mirror);

  const build = (which) => {
    // 语音重音只放大「离开静息」的那部分，不放大静息本身
    const u = { ...upper[which] }, l = { ...lower[which] }, p = { ...palm[which] };
    for (const k in REST) {
      u[k] = REST[k] + (u[k] - REST[k]) * gain;
      l[k] = REST[k] + (l[k] - REST[k]) * gain;
      p[k] = REST[k] + (p[k] - REST[k]) * gain;
    }
    const out = compose(u, l, p);
    out.ux += READY_BIAS.ux;
    out.efwd += READY_BIAS.efwd;
    out.inward += READY_BIAS.inward;
    micro(out, t, which, a, mirror);
    return out;
  };
  return { left: build('L'), right: build('R'), energy };
}

// 供待机 / 欢迎 / 指向 / 普通问答复用同一套站姿，避免切换时跳成另一个怪姿势
export const REST_POSE = { ...REST };
export const POSE_LIBRARY = POSES;
export const GESTURE_CYCLE = CYCLE;
export const GESTURE_STROKES = STROKES;
