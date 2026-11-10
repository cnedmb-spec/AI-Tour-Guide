import * as THREE from 'three';
import { createWorld, preloadModels } from './scene.js';
import { Avatar } from './avatar.js';
import { Speaker, Mic } from './audio.js';
import { createUI } from './ui.js';
import { createShotTray } from './images.js';

const $ = (id) => document.getElementById(id);
const canvas = $('scene');
const speaker = new Speaker();

let currentExhibit = null;
let speaking = false;
let firstInteract = false;
let mic = null;
let micBusy = false;
let pendingStop = false;
let lastPartial = '';
let finalText = '';
let lastReply = null;
let degraded = false;
let mainRaf = 0;   // 主渲染循环句柄：降级时一并取消，避免空转调度（DEF-27）

// 会话标识：让后端把大模型上下文按访客隔离，避免多标签/多访客互相串话
const SID = (() => {
  try {
    let s = sessionStorage.getItem('qx_sid');
    if (!s) {
      s = (crypto.randomUUID && crypto.randomUUID()) || `${Date.now()}-${Math.random().toString(36).slice(2)}`;
      sessionStorage.setItem('qx_sid', s);
    }
    return s;
  } catch (e) {
    // sessionStorage 被禁用时退回随机会话：每个标签页独立，不再共享同一条上下文（OPT-03）
    return `s-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 8)}`;
  }
})();

// 其他标签页切换主题后（写入 qx_theme_ver），本页自动刷新对齐后端主题（DEF-08）
window.addEventListener('storage', (e) => {
  if (e.key === 'qx_theme_ver' && e.newValue) location.reload();
});

// 并发控制：新提问会中断旧请求，旧请求的结果一律丢弃
let askSeq = 0;
let askCtrl = null;
let lastQuestion = '';
let lastImages = [];
let pendingQuestion = '';
let lastAskAt = 0;
let lastNudgeAt = 0;
let rateCooldownUntil = 0;   // 命中后端 429 后的本地冷却截止时间戳（DEF-30）

// ─────────── 加载主题与展品 ───────────
$('loadTip').textContent = '加载主题…';
let theme = null;
try {
  const r = await fetch(`/api/theme?t=${Date.now()}`, { cache: 'no-store' });
  if (!r.ok) throw new Error('HTTP ' + r.status);
  theme = await r.json();
  if (!theme || !theme.palette) throw new Error('主题数据不完整');
} catch (e) {
  bootFailed('本地服务未启动或返回异常：请双击桌面「启动.vbs」，等浏览器自动打开后刷新本页');
  // 模块就停在这里，控制台不要再抛未捕获异常
  await new Promise(() => {});
}
// 主题包结构校验（DEF-14）：缺关键字段时下面的顶层代码会抛未捕获异常，
// 页面永久卡在加载层——这里先拦下来给出明确的失败提示与重载按钮
if (!theme || typeof theme !== 'object' || !theme.palette
    || typeof theme.palette.accent !== 'string'
    || !theme.assistant || typeof theme.assistant.name !== 'string'
    || !Array.isArray(theme.exhibits)) {
  bootFailed('主题包数据不完整：缺少 palette.accent / assistant.name / exhibits，请检查 themes/<主题>/theme.json');
  await new Promise(() => {});
}
const themeList = await fetch('/api/themes')
  .then((r) => (r.ok ? r.json() : []))
  .catch(() => []);
const guide = await fetch('/api/guide', { cache: 'no-store' })
  .then((r) => (r.ok ? r.json() : null))
  .catch(() => null);

// 讲解员外观：默认使用吉祥物模型（用户提供的 GLB 经 tools/build_mascot.py 分件）。
// 想切回原人形 VRM，把 theme.json 的 assistant.model 写成 "vrm"，或预览时加 ?avatar=vrm。
// 无论哪种，骨架都仍是 avatar.vrm，手势 / 待机 / 视线 / 讲解链路完全不变。
const AVATAR_STYLE = (
  new URLSearchParams(location.search).get('avatar') === 'vrm' ||
  (theme.assistant && theme.assistant.model === 'vrm')
) ? 'vrm' : 'mascot';

// 数字人和吉祥物 GLB 并行下载：VRM 10.8 MB + 吉祥物 47 MB，串行会白等好几秒
const vrmReq = fetch('/assets/models/avatar.vrm')
  .then(async (r) => (r.ok ? { buffer: await r.arrayBuffer() } : { error: 'HTTP ' + r.status }))
  .catch((e) => ({ error: String(e) }));
const mascotReq = AVATAR_STYLE === 'mascot'
  ? fetch('/assets/models/mascot.glb')
    .then(async (r) => (r.ok ? { buffer: await r.arrayBuffer() } : { error: 'HTTP ' + r.status }))
    .catch((e) => ({ error: String(e) }))
  : Promise.resolve(null);

document.documentElement.style.setProperty('--accent', theme.palette.accent);
$('title').textContent = theme.name;
$('subtitle-brand').textContent = `AI GUIDE · ${theme.assistant.name}`;

// 外部展品模型不挡首屏：先用程序化占位件把场景搭起来，GLB 后台下来一件换一件
const models = new Map();

// ─────────── 3D 能力检测与降级 ───────────
function webglReady() {
  try {
    const c = document.createElement('canvas');
    return !!(c.getContext('webgl2') || c.getContext('webgl'));
  } catch (e) {
    return false;
  }
}

const NOOP_AVATAR = {
  vrm: null, basePos: { x: 0, y: 0, z: 0 },
  update() {}, setSpeaking() {}, stopSpeaking() {}, speakViseme() {}, setEmotion() {},
  setGesture() {}, faceTowards() {}, lookAtPoint() {}, clearExplainTarget() {},
  setExplainTarget() {}, resetFacing() {}, async load() {},
  bounds: () => ({ min: { y: 0 }, max: { y: 1.8 } }),
};
const NOOP_WORLD = {
  pedestals: {}, scene: null, camera: null,
  focusOn() {}, resetCamera() {}, frameAvatar() {}, attachModel() {}, setPip() {},
};

let world = NOOP_WORLD;
let avatar = NOOP_AVATAR;

if (webglReady()) {
  try {
    world = createWorld(canvas, theme, models);
    // 主题包驱动讲解员：胸牌名与「展品 → 情绪/手势」风格表都不再写死在 avatar.js
    avatar = new Avatar(world.scene, world.camera, {
      assistantName: theme.assistant && theme.assistant.name,
      exhibitStyles: theme.exhibit_styles,
      // 吉祥物模式下 VRM 只当动作骨架：从加载第一帧起就不绘制它的人形网格。
      // 旧实现等 47 MB 吉祥物解析完才隐藏，开场会先闪出旧虚拟人物。
      mascotMode: AVATAR_STYLE === 'mascot',
    });
  } catch (e) {
    console.error('3D 场景初始化失败，已降级：', e);
    world = NOOP_WORLD;
    avatar = NOOP_AVATAR;
    degrade3D('3D 场景初始化失败');
  }
} else {
  degrade3D('当前浏览器未启用 WebGL');
}
window.world = world;
window.avatar = avatar;

// 运行中显卡上下文丢失：切 2D 提示，别留一块黑屏
canvas.addEventListener('webglcontextlost', (e) => {
  e.preventDefault();
  degrade3D('3D 渲染中断（显卡上下文丢失）');
});
canvas.addEventListener('webglcontextrestored', () => {
  if (!degraded) return;
  // 上下文反复丢失/恢复时不要陷入无限刷新：5 分钟内最多自动刷 2 次，超出交给人工处理
  let rec = { n: 0, t: 0 };
  try { rec = JSON.parse(sessionStorage.getItem('gl_restore') || '') || rec; } catch (e) { /* 损坏则重置 */ }
  const nowTs = Date.now();
  if (nowTs - rec.t > 300000) rec.n = 0;
  rec.n += 1;
  rec.t = nowTs;
  try { sessionStorage.setItem('gl_restore', JSON.stringify(rec)); } catch (e) { /* 忽略 */ }
  if (rec.n > 2) {
    ui.toast('3D 反复中断，已停止自动刷新；请手动刷新页面或检查显卡/驱动', 'error');
    return;
  }
  // 重建 renderer 的成本和风险都高于刷新；先给个过渡提示，别让评委看见白屏闪一下
  ui.toast('正在恢复 3D 画面…', 'info');
  setTimeout(() => location.reload(), 1200);
});

function degrade3D(reason) {
  if (degraded) return;
  degraded = true;
  cancelAnimationFrame(mainRaf);   // 主循环也一并停掉
  console.warn('[降级] ' + reason);
  // 停掉 3D 渲染循环、resize 监听并释放 GPU 资源，别让降级后的黑幕底下还在空转
  if (world && typeof world.destroy === 'function') { try { world.destroy(); } catch (e) { /* 忽略 */ } }
  world = NOOP_WORLD;
  window.world = world;
  canvas.style.display = 'none';
  const v = document.querySelector('.vignette');
  if (v) v.style.display = 'none';
  $('fallback').classList.add('show');
  $('fallback').querySelector('.why').textContent = reason;
  const rt = $('readyTag');
  if (rt) rt.classList.remove('on');
}

// ─────────── UI ───────────
const shots = createShotTray();
const ui = createUI({
  theme,
  themeList,
  guide,
  handlers: {
    ask: (t) => ask(t),
    hasImages: () => shots.has(),
    stop: () => stopAll(),
    replay: () => replay(),
    clear: () => clearChat(),
  },
});

// 首屏闸门：吉祥物 GLB（47 MB）解析完成前不揭开加载层。
// 旧实现在这里立刻 hide，加载层淡出的 0.6 秒里正好把还没替换外观的 VRM 人形
// 露出来，开场闪一下「之前的虚拟人物」。现在由 revealScene() 统一收口；
// 同时 VRM 网格从 _setup 起就是隐藏的，两处都挡住旧人形。
let sceneRevealed = false;
function revealScene() {
  if (sceneRevealed) return;
  sceneRevealed = true;
  $('loadTip').textContent = '讲解员已就位';
  $('loading').classList.add('hide');
  ui.setStatus('待机');
  ui.loadStats();
  setInterval(() => ui.loadStats(), 8000);
  setTimeout(() => {
    if (lastAskAt) return;   // 已经有人提问了，别把欢迎语盖在回答上
    ui.showSubtitle((guide && guide.welcome) || theme.welcome || '', theme.assistant.name, 9);
  }, 900);
}
// 兜底：模型异常慢或 3D 降级时不能把观众一直关在加载层里
setTimeout(() => revealScene(), 8000);
// 展品 GLB 后台加载：到位一件换一件，长时间下不完就保留占位件，不回来拖首屏
const wantModels = (theme.exhibits || []).filter((e) => e.model).length;
let modelsIn = 0;
let modelsTried = 0;
let modelsFailed = 0;
let avatarPending = avatar !== NOOP_AVATAR;
function updateReadyTag() {
  const el = $('readyTag');
  if (!el) return;
  const parts = [];
  if (!degraded && modelsTried < wantModels) parts.push(`展品就位 ${modelsIn}/${wantModels}`);
  if (!degraded && avatarPending) parts.push('讲解员加载中');
  el.textContent = parts.join(' · ');
  el.classList.toggle('on', parts.length > 0);
}
updateReadyTag();
preloadModels(theme, {
  onModel: (id, root) => {
    models.set(id, root);
    if (world !== NOOP_WORLD) world.attachModel(id, root);
    modelsIn += 1;
    modelsTried += 1;
    updateReadyTag();
  },
  onError: () => {
    modelsFailed += 1;
    modelsTried += 1;
    updateReadyTag();
  },
}).then((m) => {
  modelsIn = Math.max(modelsIn, m.size);
  modelsTried = wantModels;
  updateReadyTag();
  if (modelsFailed) ui.toast(`${modelsFailed} 件展品模型没加载成功，先用占位件讲解`, 'error');
  console.info(`[model] 外部展品 ${m.size}/${wantModels} 件已就位`);
}).catch((e) => console.warn('[model] 展品模型加载异常：', e));
// 非安全上下文（局域网 IP / 公网 IP / 主机名）下浏览器不给麦克风，提前讲清楚
if (!window.isSecureContext) {
  const isLocal = ['localhost', '127.0.0.1', '::1'].includes(location.hostname);
  ui.showBanner(isLocal
    ? `当前不是安全上下文，麦克风不可用；请用 http://localhost${location.port ? ':' + location.port : ''} 打开`
    : '当前通过公网地址访问，浏览器不开放麦克风；可以先用文字提问，语音需要 https 或在本机打开');
}

// 数字人后台加载；失败就切无人物模式，不影响讲解
(async () => {
  if (avatar === NOOP_AVATAR) { revealScene(); return; }
  let mascotReady = false;
  try {
    const vr = await vrmReq;
    if (vr.error) throw new Error(vr.error);
    await avatar.loadBuffer(vr.buffer);
    // 待机动画是可选增强：没有文件/解析失败都会自动退回程序化待机
    await avatar.loadIdle('/assets/models/idle.vrma');
    if (AVATAR_STYLE === 'mascot') {
      // 吉祥物外观是可选增强：缺文件 / 解析失败时把隐藏的人形网格恢复回来，
      // 保证现场至少还有一个讲解员，不会因为换形象把整个数字人弄没。
      try {
        const m = await mascotReq;
        if (m && m.error) throw new Error(m.error);
        if (m && m.buffer) {
          await avatar.loadMascot(m.buffer);
          mascotReady = !!avatar.mascot;
        }
      } catch (e) {
        console.warn('[avatar] 吉祥物模型不可用，恢复 VRM 讲解员:', e.message || e);
      }
      if (!mascotReady && typeof avatar.showVrmMeshes === 'function') avatar.showVrmMeshes();
    }
    if (world !== NOOP_WORLD) frameAvatar();
  } catch (e) {
    console.error('VRM 加载失败，改用无人物模式：', e);
    avatar = NOOP_AVATAR;
    window.avatar = avatar;
    world.resetCamera();
    document.body.classList.add('no-avatar');
    ui.toast('数字人模型加载失败，讲解与问答仍可用', 'error');
  } finally {
    avatarPending = false;
    updateReadyTag();
    revealScene();
  }
})();

// 数字人 10 MB 左右，公网隧道上会慢：给一句解释，别让它看起来像卡死
setTimeout(() => {
  if (avatarPending && !degraded) ui.toast('讲解员模型较大，仍在加载；讲解和问答可以先用');
}, 15000);

function bootFailed(msg) {
  const el = $('loading');
  // 用 DOM + textContent 组装，不往 innerHTML 里拼字符串
  el.innerHTML = '';
  const box = document.createElement('div');
  box.style.cssText = 'text-align:center;line-height:2;letter-spacing:.1em';
  const head = document.createElement('div');
  head.style.cssText = 'color:#e0897c;margin-bottom:10px';
  head.textContent = '页面启动失败';
  const body = document.createElement('div');
  body.style.cssText = 'font-size:12px;color:#8a8578;max-width:520px';
  body.textContent = msg;
  const btn = document.createElement('button');
  btn.textContent = '重新加载';
  btn.style.cssText = 'margin-top:18px;padding:9px 20px;font-size:12px;background:#d9a85f;color:#0a0c0f;border:none;border-radius:2px;cursor:pointer';
  btn.onclick = () => location.reload();
  box.append(head, body, btn);
  el.appendChild(box);
}

// 人物加载完成后按真实包围盒取景，保证全身（含手和脚）在画面内
function frameAvatar() {
  if (!avatar.vrm || !world.camera) return;
  const box = avatar.bounds();
  world.frameAvatar(box);
  requestAnimationFrame(() => requestAnimationFrame(() => {
    const cam = world.camera;
    if (!cam) return;
    cam.updateMatrixWorld(true);
    const b = avatar.bounds();
    console.info('[frame] h', (b.max.y - b.min.y).toFixed(2), 'cam',
      cam.position.toArray().map((n) => n.toFixed(2)).join(','));
  }));
}

// 首次交互解锁音频
document.addEventListener('pointerdown', async () => {
  if (firstInteract) return;
  firstInteract = true;
  await speaker.unlock().catch(() => {});
  ui.toast('语音仅经本机回环处理，不出本机、不落盘');
}, { once: true });

// ─────────── 讲解主流程 ───────────
// 同步受理闸门：返回 true 表示本次提交已被受理，调用方此时才可以清空输入框。
// 此前「先清空输入再调用」在防连点吞掉提交时会把用户输入弄丢（DEF-05）。
function ask(text, retryImages, opts = {}) {
  text = (text || '').trim();
  const trayHas = shots.has();
  if (!text && !trayHas && !(retryImages && retryImages.length)) return false;
  const now = performance.now();
  // 命中限流后的冷却期：连「重试」也不再发请求，否则只会再吃一个 429（DEF-30）
  const cooldownLeft = Math.ceil((rateCooldownUntil - Date.now()) / 1000);
  if (cooldownLeft > 0) {
    if (now - lastNudgeAt > 1200) {
      lastNudgeAt = now;
      ui.toast(`提问太频繁，请等 ${cooldownLeft} 秒`);
    }
    return false;
  }
  if (!opts.force && now - lastAskAt < 400) {
    if (now - lastNudgeAt > 1200) {
      lastNudgeAt = now;
      ui.toast('刚提交过一题，稍等一下');
    }
    return false;
  }
  lastAskAt = now;
  const images = trayHas ? shots.take() : (retryImages || []);
  if (!text) text = '看看这张图，说说这是什么';
  lastImages = images;
  runAsk(text, images);
  return true;
}

async function runAsk(text, images) {
  // 新提问：掐掉上一轮的声音与请求，保证一问一答
  const seq = ++askSeq;
  if (askCtrl) askCtrl.abort();
  askCtrl = new AbortController();
  const ctrl = askCtrl;
  // 超时预算（DEF-09）：后端最坏耗时 = LLM_TIMEOUT 45s + Piper 合成数秒 + 大 JSON 传输，
  // 前端给 70 秒余量；此前 45s 与后端 LLM 超时相等，长回答必然被掐死在半路
  const timer = setTimeout(() => ctrl.abort('timeout'), 70000);
  // 长句合成要几秒：先给个进度提示，别让人以为卡死
  const hintTimer = setTimeout(() => {
    if (seq === askSeq) ui.setStatus('仍在生成语音…', 'think');
  }, 5000);

  await speaker.unlock().catch(() => {});
  stopSpeaking();
  ui.hideError();
  ui.clearSubtitle();
  ui.setStatus(images.length ? '看图中' : '思考中', 'think');
  ui.setBusy(true);
  lastQuestion = text;
  // 上一问还没答完就被新提问顶掉：记一条「已取消」，不让它凭空消失
  if (pendingQuestion && pendingQuestion !== text) ui.pushCancelled(pendingQuestion);
  pendingQuestion = text;

  let res = null;
  try {
    const r = await fetch('/api/ask', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ text, exhibit: currentExhibit, images, session: SID }),
      signal: ctrl.signal,
    });
    // 限流是「服务正常但拒绝受理」，不能报成连接失败；
    // 冷却期内连重试按钮也不发请求，否则只会继续 429（DEF-30）
    if (r.status === 429) {
      let wait = Number(r.headers.get('Retry-After')) || 0;
      let detail = '';
      try {
        const info = await r.json();
        detail = info.error || '';
        if (!wait) wait = Number(info.retry_after) || 0;
      } catch (_) { /* 响应体非 JSON 时用响应头 */ }
      if (!wait || wait < 1) wait = 5;
      rateCooldownUntil = Date.now() + wait * 1000;
      if (seq === askSeq) {
        pendingQuestion = '';
        ui.setStatus('提问过于频繁', 'err');
        ui.showError(`提问太频繁，约 ${wait} 秒后可继续${detail ? '（' + detail + '）' : ''}`, null);
        setTimeout(() => { if (seq === askSeq) ui.hideError(); }, wait * 1000);
      }
      return;
    }
    if (!r.ok) {
      let detail = '';
      try { detail = (await r.json()).error || ''; } catch (_) { /* 忽略 */ }
      throw new Error(`HTTP ${r.status}${detail ? ' ' + detail : ''}`);
    }
    res = await r.json();
  } catch (e) {
    if (seq === askSeq) {
      pendingQuestion = '';
      const timeout = e && (e.name === 'AbortError' || e === 'timeout');
      const msg = String((e && e.message) || e);
      // 只有网络层取不到响应才叫「连接失败」；HTTP 4xx/5xx 是服务已响应但拒绝或出错
      const netFail = e instanceof TypeError || /Failed to fetch|NetworkError|Load failed/i.test(msg);
      const label = timeout ? '服务未响应' : (netFail ? '服务未响应' : '服务返回异常');
      const errText = timeout
        ? (images.length ? '看图回答 70 秒未完成（视觉模型可能还在加载）' : '本地服务 70 秒未响应（大模型或语音合成可能卡住）')
        : (netFail ? '本地服务连接失败' : `本地服务返回异常（${msg}）`);
      ui.setStatus(label, 'err');
      ui.showError(errText, () => ask(lastQuestion, lastImages, { force: true }));
    }
    return;
  } finally {
    // 不论成功、失败、超时，这里一定解锁：回答文本一到就能追问
    clearTimeout(timer);
    clearTimeout(hintTimer);
    if (seq === askSeq) ui.setBusy(false);
  }
  if (seq !== askSeq) return;          // 已经被下一问取代
  pendingQuestion = '';

  // 响应字段兜底：服务端异常/字段缺失时不再把 undefined 渲染到界面
  res = res || {};
  if (!res.reply || typeof res.reply !== 'string' || !res.reply.trim()) {
    ui.setStatus('服务返回异常', 'err');
    ui.showError('回答内容为空（本地服务返回异常），可点击重试', () => ask(lastQuestion, lastImages, { force: true }));
    return;
  }
  res.elapsed_ms = Number(res.elapsed_ms) || 0;
  res.duration = Number(res.duration) || 0;
  res.visemes = Array.isArray(res.visemes) ? res.visemes : [];
  res.audio = res.audio || '';
  // 长音频：后端不再内联 base64，改用 /api/audio/{name} 拉二进制（DEF-24）
  if (!res.audio && res.audio_name) {
    try {
      const ar = await fetch(`/api/audio/${encodeURIComponent(res.audio_name)}`);
      if (ar.ok) res.audio = await ar.arrayBuffer();
    } catch (e) { /* 保持为空，走「语音合成失败」提示 */ }
  }

  currentExhibit = res.exhibit || currentExhibit;
  if (res.exhibit && world.pedestals[res.exhibit]) {
    const p = world.pedestals[res.exhibit];
    world.focusOn(res.exhibit);
    avatar.faceTowards(p.pos);
    avatar.lookAtPoint(p.pos);
    avatar.setExplainTarget(p.pos, res.exhibit);
    // 风格表给的是「展品默认性格」；后端明确判出的强情绪（欢迎、赞叹等）优先
    if (res.emotion && res.emotion !== 'neutral') avatar.setEmotion(res.emotion);
  } else {
    world.resetCamera();
    avatar.resetFacing();
    avatar.lookAtPoint({ x: 0, y: 1.55, z: 3.6 });
    // 后端返回的 emotion 此前一直没被消费，欢迎/闲聊/兜底的表情其实没生效
    avatar.setEmotion(res.emotion || 'neutral');
    avatar.setGesture(res.gesture, 2.2);
  }
  avatar.setSpeaking(true);
  speaking = true;
  ui.setStatus('讲解中', 'speak');
  ui.showSubtitle(res.reply, theme.assistant.name, res.duration);
  ui.setElapsed(res.elapsed_ms);
  ui.setPlaying(true);

  lastReply = { text: res.reply, audio: res.audio, visemes: res.visemes, duration: res.duration, exhibit: res.exhibit };
  ui.setReplayable(true);
  // 回答一到就记进问答记录，不等语音播完（评委随时能翻）
  ui.pushQA({
    q: text,
    a: res.reply,
    exhibit: res.exhibit ? (theme.exhibits.find((e) => e.id === res.exhibit) || {}).name : '',
    elapsed: res.elapsed_ms,
  });

  // 音频可能损坏（空串/编码错）：单独兜底，别让它把界面锁死在“讲解中”
  if (res.audio) {
    try {
      await speaker.play(res.audio, res.visemes, (v, amp) => {
        if (v) avatar.speakViseme(v, amp);
        else avatar.stopSpeaking();
      });
    } catch (e) {
      console.error('语音播放失败：', e);
      ui.toast('语音播放失败，字幕已显示', 'error');
    }
  } else {
    ui.toast('语音合成失败，已显示字幕', 'error');
  }
  if (seq !== askSeq) return;   // 播到一半被新问题打断
  // 音频真实结束（或解码失败）时对齐字幕；被打断时已由 stopSpeaking 清屏并在上面返回
  ui.finishSubtitle();

  speaking = false;
  avatar.stopSpeaking();
  avatar.setSpeaking(false);
  avatar.setEmotion('neutral');
  ui.setStatus('待机');
  ui.setPlaying(false);

  // 语音结束后尽快把镜头拉回全景：原来延后 1800ms 会留下约 2 秒空场，
  // 且复位为直线插值、会穿过中央的数字人。现在配合 scene.js 的绕行运镜，
  // 只留 350ms 让尾音收干净。
  setTimeout(() => {
    if (speaking) return;
    world.resetCamera();
    avatar.resetFacing();
    avatar.lookAtPoint({ x: 0, y: 1.55, z: 3.6 });
    avatar.clearExplainTarget();
  }, 350);
}

function stopSpeaking() {
  speaker.stop();
  avatar.stopSpeaking();
  avatar.setSpeaking(false);
  avatar.setEmotion('neutral');
  speaking = false;
  ui.setBusy(false);
  ui.clearSubtitle();      // 停播即清字幕，避免残句留在屏幕上
  ui.setStatus('待机');
  ui.setPlaying(false);
}

// 用户主动停止：连同在途请求一起取消（DEF-06）。
// 只调 stopSpeaking() 的话，正在生成中的回答返回后仍会照常播出。
function stopAll() {
  askSeq += 1;
  if (askCtrl) askCtrl.abort();
  pendingQuestion = '';
  stopSpeaking();
}

async function replay() {
  if (!lastReply) return;
  const seq = askSeq;
  stopSpeaking();
  await speaker.unlock().catch(() => {});
  speaking = true;
  avatar.setSpeaking(true);
  ui.setStatus('讲解中', 'speak');
  ui.setPlaying(true);
  ui.showSubtitle(lastReply.text, theme.assistant.name, lastReply.duration);
  try {
    await speaker.play(lastReply.audio, lastReply.visemes, (v, amp) => {
      if (v) avatar.speakViseme(v, amp);
      else avatar.stopSpeaking();
    });
  } catch (e) {
    console.error('语音播放失败：', e);
    ui.toast('语音播放失败，字幕已显示', 'error');
  } finally {
    // 重播期间若来了新提问（askSeq 变化），状态归新提问管，这里不能再覆盖
    if (seq === askSeq) {
      speaking = false;
      avatar.stopSpeaking();
      avatar.setSpeaking(false);
      ui.setStatus('待机');
      ui.setPlaying(false);
    }
  }
}

function clearChat() {
  askSeq++;                       // 作废在途请求
  if (askCtrl) askCtrl.abort();
  pendingQuestion = '';
  shots.clear();                  // 「重置全场」语义包含清掉待发图片（DEF-15）
  stopSpeaking();
  lastReply = null;
  ui.setReplayable(false);
  currentExhibit = null;
  world.resetCamera();
  avatar.resetFacing();
  avatar.clearExplainTarget();
  avatar.lookAtPoint({ x: 0, y: 1.55, z: 3.6 });
  ui.clearSubtitle();
  ui.resetMetrics();
  ui.setStatus('待机');
}

// ─────────── 语音输入 ───────────
async function startTalk() {
  // 主机版未接入离线识别：明确告知，避免「按住说话没反应」像坏了
  if (window.__asrAvailable === false) {
    ui.toast('演示版请用文字提问（离线语音识别未启用）', 'error');
    return;
  }
  if (speaking) stopSpeaking();
  await speaker.unlock().catch(() => {});
  if (mic || micBusy) return;
  micBusy = true;
  pendingStop = false;
  ui.setStatus('聆听中', 'listen');
  $('mic').classList.add('rec');
  ui.clearSubtitle();
  lastPartial = '';
  finalText = '';

  const m = new Mic();
  try {
    await m.start(
      (t) => { lastPartial = t; ui.partial(t || '…'); },
      (t) => { finalText = t; ui.partial(t); },
      (t) => ui.setStatus('识别异常：' + t, 'err')
    );
    mic = m;
    // 服务端有 30 秒硬上限（超限会静默丢音频）：前端 28 秒先自动收尾并提示（DEF-19）
    clearTimeout(talkGuard);
    talkGuard = setTimeout(() => {
      if (mic === m) { ui.toast('已达最长录音时长，自动结束', 'error'); stopTalk(); }
    }, 28000);
  } catch (e) {
    const name = e && e.message;
    const denied = e && (e.name === 'NotAllowedError' || e.name === 'SecurityError');
    const noService = name === 'ASR_TIMEOUT' || name === 'ASR_UNREACHABLE' || name === 'ASR_CLOSED';
    ui.setStatus(denied ? '麦克风被拒绝' : noService ? '语音服务未启动' : '麦克风不可用', 'err');
    ui.toast(
      denied ? '麦克风权限被拒绝：请用下方文字输入，或在地址栏重新授权'
        : noService ? '语音识别服务未启动：可先用文字输入，稍后再试'
          : !window.isSecureContext ? '当前不是安全上下文，请改用 http://localhost:8765'
            : '麦克风不可用，可改用文字输入',
      'error',
    );
    await m.stop().catch(() => {});
  } finally {
    micBusy = false;
    $('mic').classList.remove('rec');
    if (pendingStop) stopTalk();
  }
}

async function stopTalk() {
  clearTimeout(talkGuard);
  talkGuard = 0;
  $('mic').classList.remove('rec');
  if (micBusy) { pendingStop = true; return; }
  if (!mic) return;
  const m = mic;
  mic = null;
  ui.setStatus('识别中', 'think');

  const done = new Promise((resolve) => {
    const t0 = performance.now();
    const check = () => {
      if (finalText || performance.now() - t0 > 3000) return resolve(finalText || lastPartial);
      setTimeout(check, 80);
    };
    check();
  });
  await m.flush().catch(() => {});
  const text = await done;
  await m.stop().catch(() => {});
  finalText = '';
  lastPartial = '';
  const t = (text || '').trim();
  if (t) ask(t);
  else {
    ui.setStatus('没听清，再说一次', 'err');
    ui.clearSubtitle();
    setTimeout(() => ui.setStatus('待机'), 1800);
  }
}

// ─────────── 键盘与鼠标 ───────────
const micBtn = $('mic');
let talkGuard = 0;   // 最长录音时长定时器（服务端 30 秒硬上限，提前 2 秒自动收尾）
micBtn.addEventListener('pointerdown', (e) => {
  e.preventDefault();
  // 指针捕获（DEF-04）：手指/鼠标移出按钮后 pointerup 仍派发到按钮上，录音一定能结束
  try { micBtn.setPointerCapture(e.pointerId); } catch (err) { /* 老内核忽略 */ }
  startTalk();
});
micBtn.addEventListener('pointerup', (e) => { e.preventDefault(); stopTalk(); });
micBtn.addEventListener('pointerleave', () => { if (mic) stopTalk(); });
// 兜底：来电/弹窗/切后台等系统打断时释放麦克风，别留下永远「聆听中」
window.addEventListener('pointercancel', () => { if (mic || micBusy) stopTalk(); });
window.addEventListener('blur', () => { if (mic || micBusy) stopTalk(); });
micBtn.addEventListener('keydown', (e) => {
  if (e.code === 'Space' || e.code === 'Enter') { e.preventDefault(); e.stopPropagation(); startTalk(); }
});
micBtn.addEventListener('keyup', (e) => {
  if (e.code === 'Space' || e.code === 'Enter') { e.preventDefault(); e.stopPropagation(); stopTalk(); }
});

// 空格键只服务「按住说话」：聚焦在按钮/链接/弹层里时放行浏览器默认行为，
// 否则键盘用户无法用空格激活任何按钮
function spaceGuardActive() {
  const el = document.activeElement;
  if (!el) return false;
  if (el.tagName === 'INPUT' || el.tagName === 'TEXTAREA' || el.isContentEditable) return false;
  if (el.tagName === 'BUTTON' || el.tagName === 'A' || el.tagName === 'SELECT') return false;
  if (el.closest && el.closest('.modal.show')) return false;
  return true;
}
window.addEventListener('keydown', (e) => {
  if (e.code === 'Space' && !e.repeat && spaceGuardActive()) {
    e.preventDefault();
    startTalk();
  }
  if (e.key === 'Escape') ui.closeSheet();
});
window.addEventListener('keyup', (e) => {
  if (e.code === 'Space' && spaceGuardActive()) {
    e.preventDefault();
    stopTalk();
  }
});

// 视线跟随鼠标
window.addEventListener('pointermove', (e) => {
  if (!avatar.vrm) return;
  const x = (e.clientX / window.innerWidth - 0.5) * 4.2;
  const y = 1.9 - (e.clientY / window.innerHeight) * 1.4;
  avatar.lookAtPoint({ x, y, z: 3.4 });
});

// 软键盘高度写入 --kb：移动端输入时把舞台与抽屉整体上移，避免输入框被键盘遮住
if (window.visualViewport) {
  const vv = window.visualViewport;
  const applyKB = () => {
    const lift = Math.max(0, Math.round(window.innerHeight - vv.height - vv.offsetTop));
    document.documentElement.style.setProperty('--kb', lift + 'px');
  };
  vv.addEventListener('resize', applyKB);
  vv.addEventListener('scroll', applyKB);
  applyKB();
}

// ─────────── 渲染循环 ───────────
let lastT = performance.now();
function loop(now) {
  const dt = Math.min((now - lastT) / 1000, 0.05);
  lastT = now;
  if (!degraded) avatar.update(dt);
  mainRaf = requestAnimationFrame(loop);
}
mainRaf = requestAnimationFrame(loop);
