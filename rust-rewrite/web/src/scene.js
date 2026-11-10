import * as THREE from 'three';
import { EffectComposer } from 'three/addons/postprocessing/EffectComposer.js';
import { RenderPass } from 'three/addons/postprocessing/RenderPass.js';
import { UnrealBloomPass } from 'three/addons/postprocessing/UnrealBloomPass.js';
import { OutputPass } from 'three/addons/postprocessing/OutputPass.js';
import { RoomEnvironment } from 'three/addons/environments/RoomEnvironment.js';
import { GLTFLoader } from 'three/addons/loaders/GLTFLoader.js';

// 外部模型（Maya 导出）预加载：theme.json 里写 "model": "models/xxx.glb" 即可
// 单件 120 秒还没下完才放弃（展台保留程序化占位）。公网隧道慢时 3.6 MB 就要 20 秒，
// 超时只是兜底，不能拿来当“首屏闸门”——首屏早就不等模型了
const MODEL_TIMEOUT = 120000;

export async function preloadModels(theme, opts = {}) {
  const { onProgress, onModel, onError } = typeof opts === 'function' ? { onProgress: opts } : opts;
  const loader = new GLTFLoader();
  const out = new Map();
  const list = (theme.exhibits || []).filter((ex) => ex.model);
  const total = list.length;
  let done = 0;
  // 公网隧道带宽有限（实测 ~300 KB/s），几件并行只会互抢带宽、单件撞超时；
  // 逐件排队，每件都能吃满可用带宽，整体反而更快
  for (const ex of list) {
    await withTimeout(loadWithRetry(loader, `/assets/${ex.model}`), MODEL_TIMEOUT)
        .then((gltf) => {
          done += 1;
          if (onProgress) onProgress(done, total);
          const root = gltf.scene;
          root.userData.scale = ex.modelScale || 1;   // 外部模型按真实米制建，默认不缩放
          const over = ex.materials || {};
          root.traverse((o) => {
            if (!o.isMesh) return;
            o.castShadow = true;
            o.receiveShadow = true;
            const mats = Array.isArray(o.material) ? o.material : [o.material];
            for (const m of mats) {
              if (!m) continue;
              m.envMapIntensity = 0.75;
              // 贴图各向异性拉满：斜着看字也不糊
              for (const k of ['map', 'normalMap', 'roughnessMap', 'metalnessMap', 'emissiveMap', 'aoMap']) {
                if (m[k]) m[k].anisotropy = 16;
              }
              // theme.json 里的材质微调：按材质名覆盖颜色/粗糙度，不动原模型文件
              const cfg = over[m.name] || {};
              if (cfg.color) m.color.set(cfg.color);
              if (cfg.flat) { m.map = null; m.needsUpdate = true; }
              if (cfg.roughness !== undefined) m.roughness = cfg.roughness;
              if (cfg.metalness !== undefined) m.metalness = cfg.metalness;
              if (cfg.env !== undefined) m.envMapIntensity = cfg.env;
              if (cfg.emissive) { m.emissive.set(cfg.emissive); m.emissiveIntensity = cfg.emissiveIntensity ?? 1; }
              // 带字材质保留 mipmap 做低通滤波，改用三线性 + 各向异性 16 选清晰层级。
              // 曾对这类材质一律关 mipmap：远景里 1024×2206 的侨批信纸被采样成
              // 一排红色短竖线加黑色噪点，大字招牌也起锯齿（DEF-31 回归修正）。
              const textLike = /sign|plaque|plate|text|label|board|letter|paper|note|stamp|匾|牌|字|纸/i.test(m.name || '');
              if (cfg.sharp || textLike) {
                const noMip = !!cfg.sharp;   // 主题包显式指定时才退回无 mipmap 的锐利模式
                for (const k of ['map', 'normalMap', 'roughnessMap', 'metalnessMap']) {
                  const t = m[k];
                  if (!t) continue;
                  t.generateMipmaps = !noMip;
                  t.minFilter = noMip ? THREE.LinearFilter : THREE.LinearMipmapLinearFilter;
                  t.magFilter = THREE.LinearFilter;
                  t.anisotropy = 16;
                  t.needsUpdate = true;
                }
              }
            }
          });
          // 单独挪动某个零件：theme.json 里写 "shift": { "材质名": [dx, dy, dz] }
          const moves = ex.shift || {};
          if (Object.keys(moves).length) {
            root.traverse((o) => {
              if (!o.isMesh || !o.material) return;
              const d = moves[o.material.name];
              if (!d) return;
              o.updateWorldMatrix(true, false);
              const q = o.getWorldQuaternion(new THREE.Quaternion());
              const qp = o.parent.getWorldQuaternion(new THREE.Quaternion()).invert();
              o.position.add(new THREE.Vector3(...d).applyQuaternion(q).applyQuaternion(qp));
            });
          }

          // 铭牌斜立：theme.json 里写 "plaque": { "material": "M_QBrass", "angle": 36 }
          // 以牌子前沿为轴抬起后沿，牌面朝观众，字才读得出来
          if (ex.plaque) {
            const { material, angle = 36, offset = [0, 0, 0] } = ex.plaque;
            const plates = [];
            root.traverse((o) => {
              if (o.isMesh && o.material && o.material.name === material) plates.push(o);
            });
            for (const o of plates) {
              o.geometry.computeBoundingBox();
              const bb = o.geometry.boundingBox;
              // 铰链取其自身坐标里的“前沿中线”，再换算到父节点坐标（GLB 常带 Z-up→Y-up 的旋转）
              o.updateWorldMatrix(true, false);
              const hinge = new THREE.Vector3(
                (bb.min.x + bb.max.x) / 2 + offset[0],
                bb.min.y + offset[1],
                bb.max.z + offset[2],
              );
              const hingeParent = o.parent.worldToLocal(o.localToWorld(hinge));
              const host = o.parent;          // 牌子原来的父节点（不要在 reparent 之后取）
              const pivot = new THREE.Group();
              pivot.position.copy(hingeParent);
              host.add(pivot);
              pivot.add(o);
              o.position.sub(hingeParent);
              pivot.rotation.x = THREE.MathUtils.degToRad(angle);

              // 楔形底座：把斜牌子下面那块三角空隙填实，否则看着像浮在半空
              const W = bb.max.x - bb.min.x;
              const D = bb.max.z - bb.min.z;
              const rad = THREE.MathUtils.degToRad(angle);
              const wedge = new THREE.Mesh(
                wedgeGeometry(W, D * Math.sin(rad), D * Math.cos(rad)),
                new THREE.MeshStandardMaterial({ color: 0x2a1a11, roughness: 0.72, metalness: 0.05 }),
              );
              wedge.position.copy(hingeParent);
              wedge.name = 'plaqueWedge';
              wedge.castShadow = true;
              wedge.receiveShadow = true;
              host.add(wedge);
            }
          }
          out.set(ex.id, root);
          if (onModel) onModel(ex.id, root);   // 到位一件就先换一件，不等其它
        })
        .catch((err) => {
          done += 1;
          if (onProgress) onProgress(done, total);
          console.warn('[model] 加载失败，保留占位件', ex.model, err);
          if (onError) onError(ex.id, err);
        });
  }
  return out;
}

// 网络抖动时重试一次；真 404 也会在第二次失败后进入占位分支
async function loadWithRetry(loader, url) {
  try {
    return await loader.loadAsync(url);
  } catch (e) {
    console.warn('[model] 首次失败，重试一次', url, e);
    await new Promise((r) => setTimeout(r, 800));
    return loader.loadAsync(url);
  }
}

// 单个 Promise 加超时：GLB 下载卡住时不连带整个首屏
function withTimeout(p, ms) {
  let t = 0;
  const timeout = new Promise((_, reject) => {
    t = setTimeout(() => reject(new Error(`加载超时 ${ms} ms`)), ms);
  });
  return Promise.race([p.finally(() => clearTimeout(t)), timeout]);
}

// 楔形块：截面是直角三角形（前沿高 0、后沿高 h），沿 X 方向拉出宽 w
function wedgeGeometry(w, h, d) {
  const x = w / 2;
  const v = [
    [-x, 0, 0], [x, 0, 0],
    [-x, h, -d], [x, h, -d],
    [-x, 0, -d], [x, 0, -d],
  ];
  const faces = [
    [0, 1, 3], [0, 3, 2],   // 斜面
    [4, 1, 0], [4, 5, 1],   // 底面
    [2, 3, 5], [2, 5, 4],   // 后立面
    [0, 2, 4],              // 左端
    [1, 5, 3],              // 右端
  ];
  const pos = [];
  for (const [a, b, c] of faces) pos.push(...v[a], ...v[b], ...v[c]);
  const geo = new THREE.BufferGeometry();
  geo.setAttribute('position', new THREE.Float32BufferAttribute(pos, 3));
  geo.computeVertexNormals();
  return geo;
}

// ─────────────────────────── 贴图 ───────────────────────────
const TEXDIR = '/assets/tex/';
const texCache = new Map();
const texLoader = new THREE.TextureLoader();

function tex(file, rx = 1, ry = 1, srgb = false) {
  const key = `${file}|${rx}|${ry}|${srgb}`;
  const hit = texCache.get(key);
  if (hit) return hit;
  const t = texLoader.load(TEXDIR + file, undefined, undefined, () => console.warn('[tex] 缺失', file));
  t.wrapS = t.wrapT = THREE.RepeatWrapping;
  t.repeat.set(rx, ry);
  t.anisotropy = 8;
  t.colorSpace = srgb ? THREE.SRGBColorSpace : THREE.NoColorSpace;
  texCache.set(key, t);
  return t;
}

// 带颜色/法线/粗糙度三张图的 PBR 材质
function surf(name, rx, ry, o = {}) {
  const opt = Object.assign({
    color: 0xffffff, metalness: 0, roughness: 1, normalScale: 1,
    normal: true, rough: true, side: THREE.FrontSide,
  }, o);
  const m = new THREE.MeshStandardMaterial({
    color: opt.color,
    metalness: opt.metalness,
    roughness: opt.roughness,
    map: tex(`${name}_color.jpg`, rx, ry, true),
    side: opt.side,
  });
  if (opt.normal) {
    m.normalMap = tex(`${name}_normal.jpg`, rx, ry);
    m.normalScale.set(opt.normalScale, opt.normalScale);
  }
  if (opt.rough) m.roughnessMap = tex(`${name}_rough.jpg`, rx, ry);
  if (opt.emissive !== undefined) {
    m.emissive = new THREE.Color(opt.emissive);
    m.emissiveIntensity = opt.emissiveIntensity ?? 1;
  }
  return m;
}

const MAT = {
  bronze: () => new THREE.MeshStandardMaterial({ color: 0x8b9a80, metalness: 0.72, roughness: 0.44 }),
  gold: () => new THREE.MeshStandardMaterial({ color: 0xd9ac5c, metalness: 1, roughness: 0.22 }),
  stone: () => surf('stone', 2, 2, { color: 0x8e8a80, roughness: 0.9, normalScale: 0.8 }),
  jade: () => new THREE.MeshStandardMaterial({ color: 0x8fbfa9, metalness: 0.2, roughness: 0.28, transparent: true, opacity: 0.9 }),
  wood: () => surf('wood', 2, 2, { color: 0x8a6a4c, roughness: 0.75, normalScale: 1.2 }),
  ember: (c) => new THREE.MeshBasicMaterial({ color: c, transparent: true, opacity: 0.9 }),
  brass: () => new THREE.MeshStandardMaterial({ color: 0xc8a45e, metalness: 0.95, roughness: 0.32 }),
  darkGlass: () => new THREE.MeshStandardMaterial({ color: 0x0d0b08, metalness: 0.55, roughness: 0.22 }),
};

// ─────────────────────────── 几何小工具 ───────────────────────────
const mesh = (geo, mat, x = 0, y = 0, z = 0) => {
  const m = new THREE.Mesh(geo, mat);
  m.position.set(x, y, z);
  return m;
};
const box = (w, h, d, mat, x = 0, y = 0, z = 0) => mesh(new THREE.BoxGeometry(w, h, d), mat, x, y, z);
const cyl = (rt, rb, h, seg, mat, x = 0, y = 0, z = 0) =>
  mesh(new THREE.CylinderGeometry(rt, rb, h, seg), mat, x, y, z);

// 暖光窗：玻璃自发光，模拟馆内透出的灯光
function warmGlass(strength = 0.55) {
  return new THREE.MeshStandardMaterial({
    color: 0x120c06,
    emissive: 0xffb168,
    emissiveIntensity: strength,
    metalness: 0.35,
    roughness: 0.2,
  });
}

// 一扇带木/石窗框的窗（8 个薄片，正面朝向 +Z）
function addWindow(parent, w, h, x, y, z, frameMat, glassMat, opts = {}) {
  const g = new THREE.Group();
  g.position.set(x, y, z);
  if (opts.rotY) g.rotation.y = opts.rotY;
  const t = 0.011;
  g.add(box(w, h, 0.012, glassMat, 0, 0, 0));
  g.add(box(t, h + t * 2, 0.022, frameMat, -w / 2 - t / 2, 0, 0.004));
  g.add(box(t, h + t * 2, 0.022, frameMat, w / 2 + t / 2, 0, 0.004));
  g.add(box(w + t * 2, t, 0.022, frameMat, 0, h / 2 + t / 2, 0.004));
  g.add(box(w + t * 2, t, 0.022, frameMat, 0, -h / 2 - t / 2, 0.004));
  g.add(box(0.004, h, 0.014, frameMat, 0, 0, 0.008));
  g.add(box(w * 0.98, 0.004, 0.014, frameMat, 0, 0, 0.008));
  if (opts.sill) g.add(box(w + t * 4, 0.014, 0.04, frameMat, 0, -h / 2 - t, 0.008));
  parent.add(g);
  return g;
}

// 券拱门洞：暗部 + 半圆拱圈 + 两侧门柱
function addArch(parent, w, h, x, y, z, stoneMat, dimMat) {
  const g = new THREE.Group();
  g.position.set(x, y, z);
  const r = w / 2;
  g.add(box(w, h, 0.02, dimMat, 0, h / 2, 0));
  const ring = mesh(new THREE.TorusGeometry(r, w * 0.05, 6, 24, Math.PI), stoneMat, 0, h, 0.012);
  g.add(ring);
  g.add(box(w * 0.075, h, 0.03, stoneMat, -r - w * 0.037, h / 2, 0.012));
  g.add(box(w * 0.075, h, 0.03, stoneMat, r + w * 0.037, h / 2, 0.012));
  parent.add(g);
  return g;
}

// 带字贴图保留 mipmap（多级渐远纹理）做低通滤波：牌子在远景只有几十像素，
// 关掉会把密集笔画采样成锯齿与摩尔纹；三线性 + 各向异性 16 兼顾平滑与清晰（DEF-31 回归修正）。
function textTexture(cv) {
  const t = new THREE.CanvasTexture(cv);
  t.colorSpace = THREE.SRGBColorSpace;
  t.generateMipmaps = true;
  t.minFilter = THREE.LinearMipmapLinearFilter;
  t.magFilter = THREE.LinearFilter;
  t.anisotropy = 16;
  return t;
}

// 展牌：米白亚光底板 + 深褐刻字（深底金字在展厅暗处会糊成一片，浅底深字才看得清）
function plateTexture(name, sub) {
  const W = 2048, H = 720;
  const cv = document.createElement('canvas');
  cv.width = W; cv.height = H;
  const g = cv.getContext('2d');

  // 底板：微微泛黄的纸白，带一点竖向刷痕
  const grad = g.createLinearGradient(0, 0, 0, H);
  grad.addColorStop(0, '#f4ecdb');
  grad.addColorStop(0.55, '#e9dfc9');
  grad.addColorStop(1, '#ded1b6');
  g.fillStyle = grad;
  g.fillRect(0, 0, W, H);
  for (let i = 0; i < 90; i++) {
    g.fillStyle = `rgba(140,118,84,${Math.random() * 0.05})`;
    g.fillRect(0, Math.random() * H, W, 1 + Math.random() * 2);
  }

  // 双线铜色边框
  g.strokeStyle = 'rgba(150,112,56,.85)';
  g.lineWidth = 10;
  g.strokeRect(28, 28, W - 56, H - 56);
  g.strokeStyle = 'rgba(150,112,56,.45)';
  g.lineWidth = 4;
  g.strokeRect(60, 60, W - 120, H - 120);

  g.textAlign = 'center';
  g.textBaseline = 'middle';
  // 展名：字号按字数自适应，四字五字都撑满又不溢出；字重加粗保住笔画
  const n = [...name].length;
  const fs = Math.min(412, Math.floor(1760 / n));
  g.fillStyle = '#1c1208';
  g.font = `800 ${fs}px "KaiTi","STKaiti","Microsoft YaHei",serif`;
  g.fillText(name, W / 2, H * 0.47);

  if (sub) {
    g.fillStyle = '#6b4a20';
    g.font = '700 96px "Times New Roman",Georgia,serif';
    g.fillText(sub, W / 2, H * 0.855);
  }
  return textTexture(cv);
}

// 骑楼店铺招牌：深色木底 + 金漆字（贴图 2 倍采样，招牌在屏幕上很小也保得住笔画）
function shopSignTexture(text) {
  const W = 1024, H = 256;
  const cv = document.createElement('canvas');
  cv.width = W; cv.height = H;
  const g = cv.getContext('2d');
  g.fillStyle = '#1d1510';
  g.fillRect(0, 0, W, H);
  g.strokeStyle = 'rgba(206,168,96,.9)';
  g.lineWidth = 10;
  g.strokeRect(20, 20, W - 40, H - 40);
  g.textAlign = 'center';
  g.textBaseline = 'middle';
  g.fillStyle = '#f2d28d';
  g.font = '800 148px "KaiTi","STKaiti","Microsoft YaHei",serif';
  g.fillText(text, W / 2, H / 2 + 4);
  return textTexture(cv);
}

// 干陈皮表面：米黄底 + 密布油胞（小凹点）+ 皱纹，靠材质颜色区分新皮/老皮
function chenpiDryTexture() {
  const S = 512;
  const cv = document.createElement('canvas');
  cv.width = cv.height = S;
  const g = cv.getContext('2d');

  const grad = g.createLinearGradient(0, 0, S, S);
  grad.addColorStop(0, '#e3cda9');
  grad.addColorStop(0.5, '#d6bd95');
  grad.addColorStop(1, '#c3a67d');
  g.fillStyle = grad;
  g.fillRect(0, 0, S, S);

  // 油胞：果皮上密密麻麻的小凹点，是陈皮最容易认的特点
  for (let i = 0; i < 9000; i++) {
    const x = Math.random() * S;
    const y = Math.random() * S;
    const r = 0.8 + Math.random() * 2.6;
    g.beginPath();
    g.arc(x, y, r, 0, Math.PI * 2);
    g.fillStyle = `rgba(92,64,36,${0.10 + Math.random() * 0.26})`;
    g.fill();
    g.beginPath();
    g.arc(x + r * 0.3, y + r * 0.3, r * 0.55, 0, Math.PI * 2);
    g.fillStyle = `rgba(255,246,222,${0.08 + Math.random() * 0.16})`;
    g.fill();
  }

  // 陈化的深浅斑块
  for (let i = 0; i < 26; i++) {
    const x = Math.random() * S;
    const y = Math.random() * S;
    const r = 12 + Math.random() * 46;
    const rg = g.createRadialGradient(x, y, 0, x, y, r);
    rg.addColorStop(0, `rgba(96,62,32,${0.06 + Math.random() * 0.10})`);
    rg.addColorStop(1, 'rgba(96,62,32,0)');
    g.fillStyle = rg;
    g.fillRect(x - r, y - r, r * 2, r * 2);
  }

  // 细皱
  g.globalAlpha = 0.12;
  g.strokeStyle = '#7c5430';
  for (let i = 0; i < 60; i++) {
    const x0 = Math.random() * S;
    const y0 = Math.random() * S;
    g.lineWidth = 0.8 + Math.random() * 1.4;
    g.beginPath();
    g.moveTo(x0, y0);
    g.bezierCurveTo(x0 + 30, y0 + 26, x0 + 66, y0 - 24, x0 + 110, y0 + 6);
    g.stroke();
  }
  g.globalAlpha = 1;

  const t = new THREE.CanvasTexture(cv);
  t.colorSpace = THREE.SRGBColorSpace;
  t.wrapS = t.wrapT = THREE.RepeatWrapping;
  t.anisotropy = 8;
  return t;
}

// 一片干陈皮：柑橘三瓣开皮的轮廓 + 边缘向内卷起成碗状 + 表面揉皱
function chenpiPeelGeo(size, curl, lobes, seed) {
  const RINGS = 8;
  const SECT = 30;
  const count = 1 + RINGS * SECT;
  const pos = new Float32Array(count * 3);
  const uv = new Float32Array(count * 2);
  const idx = [];

  const put = (i, x, y, z) => {
    pos[i * 3] = x;
    pos[i * 3 + 1] = y;
    pos[i * 3 + 2] = z;
    uv[i * 2] = 0.5 + x / (size * 2.3);
    uv[i * 2 + 1] = 0.5 + y / (size * 2.3);
  };

  put(0, 0, 0, 0);

  for (let r = 1; r <= RINGS; r++) {
    const t = r / RINGS;
    for (let s = 0; s < SECT; s++) {
      const a = (s / SECT) * Math.PI * 2;
      // 三瓣（或四瓣）开皮的轮廓，再拉出不规则的缺口
      const lobe = 1 + 0.10 * Math.sin(lobes * a + seed * 2.1)
        + 0.045 * Math.sin(6.7 * a + seed * 5.3);
      const rad = t * size * lobe * (1 + 0.03 * Math.sin(a * 9.1 + seed * 4.7));
      const x = Math.cos(a) * rad;
      const y = Math.sin(a) * rad;
      // 越靠外翘得越高，形成卷边；再叠一层细皱
      const z = curl * (t * t) * size * 0.55
        + Math.sin(a * 8.3 + seed * 7) * 0.0045 * t * t
        + Math.sin(t * 7.1 + seed * 3) * 0.0032 * t;
      put(1 + (r - 1) * SECT + s, x, y, z);
    }
  }

  for (let s = 0; s < SECT; s++) {
    idx.push(0, 1 + s, 1 + ((s + 1) % SECT));
  }
  for (let r = 1; r < RINGS; r++) {
    for (let s = 0; s < SECT; s++) {
      const s2 = (s + 1) % SECT;
      const a = 1 + (r - 1) * SECT + s;
      const b = 1 + (r - 1) * SECT + s2;
      const c = 1 + r * SECT + s;
      const d = 1 + r * SECT + s2;
      idx.push(a, c, d, a, d, b);
    }
  }

  const geo = new THREE.BufferGeometry();
  geo.setAttribute('position', new THREE.BufferAttribute(pos, 3));
  geo.setAttribute('uv', new THREE.BufferAttribute(uv, 2));
  geo.setIndex(idx);
  geo.computeVertexNormals();
  return geo;
}

// 叶片卡片：程序化画一片带主脉的尖叶，比抠图贴图干净，远看也不会糊成纸片
function leafCardTexture() {
  const W = 128, H = 160;
  const cv = document.createElement('canvas');
  cv.width = W; cv.height = H;
  const g = cv.getContext('2d');

  const grad = g.createLinearGradient(0, H, W, 0);
  grad.addColorStop(0, '#33491f');
  grad.addColorStop(0.5, '#4e6631');
  grad.addColorStop(1, '#688442');
  g.fillStyle = grad;
  g.beginPath();
  g.moveTo(W * 0.5, H * 0.03);
  g.bezierCurveTo(W * 0.96, H * 0.28, W * 0.92, H * 0.72, W * 0.5, H * 0.98);
  g.bezierCurveTo(W * 0.08, H * 0.72, W * 0.04, H * 0.28, W * 0.5, H * 0.03);
  g.closePath();
  g.fill();

  g.strokeStyle = 'rgba(32,48,20,.5)';
  g.lineWidth = 3;
  g.beginPath();
  g.moveTo(W * 0.5, H * 0.07);
  g.lineTo(W * 0.5, H * 0.94);
  g.stroke();

  g.lineWidth = 1.6;
  for (let i = 1; i < 7; i++) {
    const y = H * (0.09 + i * 0.125);
    g.beginPath();
    g.moveTo(W * 0.5, y);
    g.lineTo(W * 0.24, y + H * 0.065);
    g.moveTo(W * 0.5, y);
    g.lineTo(W * 0.76, y + H * 0.065);
    g.stroke();
  }

  const t = new THREE.CanvasTexture(cv);
  t.colorSpace = THREE.SRGBColorSpace;
  t.anisotropy = 8;
  return t;
}

// 十字交叉的叶片：两片互相垂直，从任何角度看都不会只剩一条边
function crossLeafGeo(w, h) {
  const a = new THREE.PlaneGeometry(w, h);
  const b = new THREE.PlaneGeometry(w, h);
  b.rotateY(Math.PI / 2);
  const geo = new THREE.BufferGeometry();
  const cat = (name, size) => new THREE.Float32BufferAttribute([
    ...a.attributes[name].array, ...b.attributes[name].array,
  ], size);
  geo.setAttribute('position', cat('position', 3));
  geo.setAttribute('normal', cat('normal', 3));
  geo.setAttribute('uv', cat('uv', 2));
  const offset = a.attributes.position.count;
  geo.setIndex([...a.index.array, ...b.index.array.map((i) => i + offset)]);
  a.dispose();
  b.dispose();
  return geo;
}

// 柔和接触阴影（人物与展台脚下）
function blobShadow() {
  const cv = document.createElement('canvas');
  cv.width = cv.height = 128;
  const g = cv.getContext('2d');
  const rg = g.createRadialGradient(64, 64, 4, 64, 64, 62);
  rg.addColorStop(0, 'rgba(0,0,0,.85)');
  rg.addColorStop(0.55, 'rgba(0,0,0,.35)');
  rg.addColorStop(1, 'rgba(0,0,0,0)');
  g.fillStyle = rg;
  g.fillRect(0, 0, 128, 128);
  const t = new THREE.CanvasTexture(cv);
  return new THREE.Mesh(
    new THREE.PlaneGeometry(1, 1),
    new THREE.MeshBasicMaterial({ map: t, transparent: true, depthWrite: false, opacity: 0.55 })
  );
}

export function createWorld(canvas, theme, models = null) {
  const renderer = new THREE.WebGLRenderer({ canvas, antialias: true, powerPreference: 'high-performance' });
  renderer.setPixelRatio(Math.min(window.devicePixelRatio, 2));
  renderer.setSize(window.innerWidth, window.innerHeight);
  renderer.toneMapping = THREE.ACESFilmicToneMapping;
  renderer.toneMappingExposure = 0.98;
  renderer.shadowMap.enabled = true;
  renderer.shadowMap.type = THREE.PCFShadowMap;

  const scene = new THREE.Scene();
  scene.background = new THREE.Color(theme.palette.bg);
  scene.fog = new THREE.FogExp2(theme.palette.fog, 0.042);

  // near 从 0.1 抬到 0.28：镜头贴近模型时不再把面片裁成可直接穿进去的空洞
  const camera = new THREE.PerspectiveCamera(34, window.innerWidth / window.innerHeight, 0.28, 120);
  camera.position.set(0, 1.5, 6.0);

  const pmrem = new THREE.PMREMGenerator(renderer);
  scene.environment = pmrem.fromScene(new RoomEnvironment(), 0.04).texture;
  scene.environmentIntensity = 0.38;

  // 后处理是离屏渲染，renderer 的 antialias 不生效；显式开 4x MSAA，边缘才不毛
  const composer = new EffectComposer(
    renderer,
    new THREE.WebGLRenderTarget(window.innerWidth, window.innerHeight, {
      type: THREE.HalfFloatType,
      samples: 4,
    }),
  );
  composer.addPass(new RenderPass(scene, camera));
  // 泛光收紧：原来半径大、阈值低，亮墙和展牌都被晕开，整体发虚
  const bloom = new UnrealBloomPass(new THREE.Vector2(window.innerWidth, window.innerHeight), 0.2, 0.35, 1.0);
  composer.addPass(bloom);
  composer.addPass(new OutputPass());

  const world = new THREE.Group();
  scene.add(world);

  const lights = buildLights(scene, theme);
  buildFloor(world, theme);
  buildHall(world, theme);

  // 人物脚下的柔影
  const shadow = blobShadow();
  shadow.rotation.x = -Math.PI / 2;
  shadow.scale.set(1.5, 1.9, 1);
  shadow.position.set(0, 0.004, 1.15);
  world.add(shadow);

  const pedestals = {};
  for (const ex of theme.exhibits) {
    const p = ex.pos || [0, 0, -2];
    const prop = (models && models.get(ex.id)) || makeProp(ex.prop || 'generic', theme.palette, ex.name);
    prop.position.set(0, 0.73, 0);
    prop.scale.setScalar(prop.userData.scale || 0.7);
    prop.updateMatrixWorld(true);

    // 展台尺寸跟着展品脚印走：宽展品配宽台面，小件配窄台，避免“小台托大楼”
    const pb = new THREE.Box3().setFromObject(prop);
    const topR = THREE.MathUtils.clamp(
      Math.max(pb.max.x - pb.min.x, pb.max.z - pb.min.z) / 2 + 0.07, 0.40, 0.54);
    const ped = buildPedestal(world, p, theme, ex.name, topR);
    ped.add(prop);

    // 展品落在台面上的接触阴影，避免“悬浮在台面上”的假感
    const foot = Math.max(pb.max.x - pb.min.x, pb.max.z - pb.min.z);
    const contact = blobShadow();
    contact.rotation.x = -Math.PI / 2;
    contact.scale.set(foot * 1.15, foot * 1.15, 1);
    contact.position.set(0, 0.845, 0);
    ped.add(contact);

    ped.updateWorldMatrix(true, true);
    pedestals[ex.id] = {
      group: ped, prop, contact, pos: new THREE.Vector3(p[0], 1.36, p[2]), rot: p[3] || 0,
      h: Math.max(0.4, pb.max.y - 0.73),
    };
    lights.each(ex, p);
  }

  // 外部 GLB 后台到位：只换展品本体，展台不动（台面尺寸本来就是 0.40–0.54 的窄区间）
  function attachModel(id, root) {
    const p = pedestals[id];
    if (!p || !root) return;
    root.position.set(0, 0.73, 0);
    root.scale.setScalar(root.userData.scale || 0.7);
    root.updateMatrixWorld(true);
    p.group.remove(p.prop);
    p.group.add(root);
    p.prop = root;
    const pb = new THREE.Box3().setFromObject(root);
    p.h = Math.max(0.4, pb.max.y - 0.73);
    const foot = Math.max(pb.max.x - pb.min.x, pb.max.z - pb.min.z);
    p.contact.scale.set(foot * 1.15, foot * 1.15, 1);
    if (focusId === id) focusOn(id);   // 正在看这件：按新高度重算机位
  }

  const dust = buildDust(world, theme);

  // 相机推拉
  const camBase = { pos: new THREE.Vector3(0, 1.5, 6.0), look: new THREE.Vector3(0, 1.0, -1.2) };
  const camTarget = { pos: camBase.pos.clone(), look: camBase.look.clone() };
  const camLook = camBase.look.clone();   // 当前视点：与相机位置一起插值，避免视点瞬跳
  let focusId = null;
  let camAnim = null;

  // ── 讲解员同步小窗 ────────────────────────────────────────────────
  // 展品特写的机位会横向让开中央人物（见 focusOn），主镜头里因此看不到讲解员。
  // 这里用同一 renderer 的第二机位把**同一个 Avatar 对象**再画一次到右下角：
  // 骨骼、手势、口型都由主循环驱动，不需要第二份模型，天然同步；
  // 主镜头、展品构图、场景布局一律不动。
  // 小窗位置与大小完全由 CSS 的 .pip-frame 决定（读 getBoundingClientRect），
  // 每帧按实际宽高重算 aspect，放大/缩小窗口或答题让位都不用碰这段渲染代码。
  // 取景到「头顶 → 大腿上段」：上一版只到腰线，手一垂就出画，出现
  // 「一只手从边缘斜伸出来、另一只不见」的残画面（051410 录屏）。
  // 现在双手无论自然下垂还是胸前比划都完整入镜，人物仍占约九成画面高度。
  //
  // 吉祥物比人形 VRM 宽得多（Q 版大头 + 两侧耳机，缩放后约 1.4m 宽 × 1.62m 高），
  // 人形用的 30° / 2.1m 机位水平视野只有 0.9m，会把两只手和耳机切掉。
  // 44° / 2.85m 机位在 .diag/mascot_motion.html 与 pip_geom.mjs 里用包围盒
  // 8 角投影验证：最坏角 |NDC| < 0.96，帽顶、耳机球、双手、鞋全部完整入画，
  // 且留出余量让抬手动作不会顶到画框。
  const pipCam = new THREE.PerspectiveCamera(44, 0.8, 0.3, 5.2);
  pipCam.position.set(0, 1.05, 4.05);
  pipCam.lookAt(0, 0.95, 1.2);   // 人物基准位置 (0, 0, 1.2)，身高约 1.62
  let pipOn = false;
  let pipFrame = document.getElementById('pipFrame');
  function setPip(on) {
    pipOn = !!on && !document.body.classList.contains('no-avatar');
    if (!pipFrame) pipFrame = document.getElementById('pipFrame');
    if (pipFrame) pipFrame.classList.toggle('show', pipOn);
  }
  function renderPip() {
    if (!pipOn || !pipFrame || document.body.classList.contains('no-avatar')) return;
    const r = pipFrame.getBoundingClientRect();
    if (r.width < 40 || r.height < 40) return;
    const x = Math.round(r.left);
    const y = Math.round(window.innerHeight - r.bottom);  // WebGL viewport 原点在左下
    if (x < 0 || y < 0) return;
    const w = Math.round(r.width), h = Math.round(r.height);
    const prevColor = renderer.getClearColor(new THREE.Color()).clone();
    const prevAlpha = renderer.getClearAlpha();
    const prevAutoClear = renderer.autoClear;
    const prevShadow = renderer.shadowMap.autoUpdate;
    // 关键修复：scene.background 是主题色（近黑），three.js 在 render() 里
    // 会用它覆盖 renderer 的 clearColor。上一版只改 setClearColor 完全没生效，
    // 060750 录屏取样仍是 (8,8,8)。这里临时替换 scene.background 才真正换底色。
    const prevBg = scene.background;
    const pipBg = new THREE.Color(0x4a3826);
    renderer.autoClear = false;
    renderer.shadowMap.autoUpdate = false;   // 阴影主渲染已算过，不重复
    renderer.setScissorTest(true);
    renderer.setViewport(x, y, w, h);
    renderer.setScissor(x, y, w, h);
    scene.background = pipBg;
    renderer.setClearColor(pipBg, 1);
    renderer.clear(true, true, false);
    pipCam.aspect = w / h;
    pipCam.updateProjectionMatrix();
    renderer.render(scene, pipCam);
    scene.background = prevBg;
    renderer.setScissorTest(false);
    renderer.setViewport(0, 0, window.innerWidth, window.innerHeight);
    renderer.setClearColor(prevColor, prevAlpha);
    renderer.autoClear = prevAutoClear;
    renderer.shadowMap.autoUpdate = prevShadow;
  }

  // 数字人站在场景中央（约 x=0、z=1.15），把它占的 xz 区域设为相机禁入区。
  // 旧实现把展品特写机位推到 z≈0.9，正好落进这块区域：讲解时人物出画，
  // 复位时相机又沿直线穿过人物模型（演示录屏 16.9–17.6s 的穿模）。
  const KEEPOUT = { minX: -1.15, maxX: 1.15, minZ: 0.25, maxZ: 2.1 };
  // Liang–Barsky 线段-矩形求交（xz 投影）：解析判定、无漏检。
  // 固定步数采样会漏掉只擦过矩形一角的路径，那正是复位穿模最难查的一类假阴性。
  function segmentHitsKeepout(a, b) {
    const dx = b.x - a.x, dz = b.z - a.z;
    const p = [-dx, dx, -dz, dz];
    const q = [a.x - KEEPOUT.minX, KEEPOUT.maxX - a.x,
               a.z - KEEPOUT.minZ, KEEPOUT.maxZ - a.z];
    let t0 = 0, t1 = 1;
    for (let i = 0; i < 4; i++) {
      if (p[i] === 0) {
        if (q[i] < 0) return false;      // 平行于该边且整段在矩形外
      } else {
        const r = q[i] / p[i];
        if (p[i] < 0) { if (r > t1) return false; if (r > t0) t0 = r; }
        else { if (r < t0) return false; if (r < t1) t1 = r; }
      }
    }
    return t0 <= t1;
  }
  function easeInOutCubic(t) {
    return t < 0.5 ? 4 * t * t * t : 1 - Math.pow(-2 * t + 2, 3) / 2;
  }
  // 所有镜头移动走同一条管线：固定时长缓动 + 同步插值视点；
  // 直线会穿过中央时改用带侧向控制点的二次贝塞尔，从人物旁边绕过去。
  function moveCameraTo(pos, look, dur = 0.95) {
    const from = camera.position.clone();
    const fromLook = camLook.clone();
    camTarget.pos.copy(pos);
    camTarget.look.copy(look);
    const side = (from.x + pos.x) >= 0 ? 1 : -1;
    const ctrl = segmentHitsKeepout(from, pos)
      ? new THREE.Vector3(
          side * Math.max(2.4, Math.abs(from.x) + 1.4, Math.abs(pos.x) + 1.4),
          Math.max(from.y, pos.y) + 0.3,
          (from.z + pos.z) * 0.5
        )
      : null;
    camAnim = { from, fromLook, to: pos.clone(), toLook: look.clone(), ctrl, dur, t: 0 };
  }

  // 按人物真实包围盒取景。0.78 会让吉祥物占画面约 78% 高度，Q 版大头与
  // 两侧耳机几乎把后排展台整排压住（0813 现场截图反馈「太大、挡住展品」）。
  // 退到 0.60 后人物约 60% 高、屏幕宽度缩小约四分之一，两侧展台完整露出，
  // 底部字幕卡依旧遮住脚部空地。展品特写用独立 pipCam 小窗，不受主镜头影响。
  function frameAvatar(box, ratio = 0.60) {
    const center = box.getCenter(new THREE.Vector3());
    const h = Math.max(0.5, box.max.y - box.min.y);
    const tan = Math.tan(THREE.MathUtils.degToRad(camera.fov) / 2);
    const d = h / (2 * ratio * tan);
    camBase.pos.set(center.x, center.y + 0.3, center.z + d);
    camBase.look.set(center.x, center.y + 0.02, center.z);
    camTarget.pos.copy(camBase.pos);
    camTarget.look.copy(camBase.look);
    camLook.copy(camBase.look);
    camAnim = null;
    camera.position.copy(camBase.pos);
    camera.lookAt(camLook);
  }

  function focusOn(id) {
    focusId = id;
    const p = pedestals[id];
    if (!p) return;
    setPip(true);   // 特写镜头让开了人物，改用同步小窗保持讲解员在场
    // 按展品实际高度反推机位，保证整件展品都进画面（高展品退远、小件推近）
    const d = THREE.MathUtils.clamp(p.h * 2.05, 1.8, 4.8);
    const dir = new THREE.Vector3().subVectors(camBase.pos, p.pos);
    dir.y = 0;
    if (dir.lengthSq() < 1e-6) dir.set(0, 0, 1);
    dir.normalize();
    const lookY = 0.73 + p.h * 0.5;
    const pos = p.pos.clone().addScaledVector(dir, d);
    // 侧前方取景：横向让开中央的数字人，且机位不越过人物所在的 z 平面。
    // 这样「推进 → 特写 → 复位」两端都在禁入区外，绕行路径才有效。
    const side = p.pos.x >= 0 ? 1 : -1;
    pos.x = side * Math.max(1.6, Math.abs(pos.x));
    pos.z = Math.min(pos.z, 0.15);
    pos.y = lookY + 0.18;
    moveCameraTo(pos, new THREE.Vector3(p.pos.x, lookY, p.pos.z), 0.85);
  }
  function resetCamera() {
    focusId = null;
    setPip(false);  // 回到全景，主镜头里本来就是讲解员全身，不需要小窗
    moveCameraTo(camBase.pos, camBase.look, 1.0);
  }

  function resize() {
    const w = window.innerWidth, h = window.innerHeight;
    camera.aspect = w / h;
    camera.updateProjectionMatrix();
    renderer.setSize(w, h);
    composer.setSize(w, h);
  }
  window.addEventListener('resize', resize);

  let lastT = performance.now();
  let tAcc = 0;
  let rafId = 0;
  function render(now) {
    const dt = Math.min((now - lastT) / 1000, 0.05);
    lastT = now;
    tAcc += dt;
    dust.rotation.y += dt * 0.01;
    let i = 0;
    for (const k in pedestals) {
      const p = pedestals[k];
      const on = focusId === k;
      p.group.userData.ring.material.opacity += ((on ? 0.95 : 0.22) - p.group.userData.ring.material.opacity) * dt * 4;
      // 轻微左右摆动而不是整圈旋转，保证展品正面始终朝向观众
      const sway = on ? 0 : Math.sin(tAcc * 0.3 + i * 1.1) * 0.2;
      p.prop.rotation.y += (sway - p.prop.rotation.y) * Math.min(1, dt * 2);
      i++;
    }
    if (camAnim) {
      const a = camAnim;
      a.t = Math.min(1, a.t + dt / a.dur);
      const e = easeInOutCubic(a.t);
      if (a.ctrl) {
        const u = 1 - e;
        camera.position.set(
          u * u * a.from.x + 2 * u * e * a.ctrl.x + e * e * a.to.x,
          u * u * a.from.y + 2 * u * e * a.ctrl.y + e * e * a.to.y,
          u * u * a.from.z + 2 * u * e * a.ctrl.z + e * e * a.to.z
        );
      } else {
        camera.position.lerpVectors(a.from, a.to, e);
      }
      camLook.lerpVectors(a.fromLook, a.toLook, e);
      if (a.t >= 1) camAnim = null;
    } else {
      camera.position.lerp(camTarget.pos, dt * 2.2);
      camLook.lerp(camTarget.look, dt * 2.2);
    }
    camera.lookAt(camLook);
    // 上一帧小窗改过 viewport/scissor，先恢复全屏再走后处理
    renderer.setScissorTest(false);
    renderer.setViewport(0, 0, window.innerWidth, window.innerHeight);
    composer.render();
    renderPip();
    rafId = requestAnimationFrame(render);
  }
  rafId = requestAnimationFrame(render);

  // 降级/上下文丢失时由外部调用：停掉渲染循环、解除监听并深度释放 GPU 资源
  function destroy() {
    cancelAnimationFrame(rafId);
    setPip(false);
    window.removeEventListener('resize', resize);
    // 遍历场景释放几何体/材质/贴图，避免降级后显存常驻（DEF-27）
    try {
      scene.traverse((o) => {
        if (o.geometry && o.geometry.dispose) o.geometry.dispose();
        const mats = Array.isArray(o.material) ? o.material : (o.material ? [o.material] : []);
        for (const m of mats) {
          if (!m) continue;
          for (const k of ['map', 'normalMap', 'roughnessMap', 'metalnessMap', 'emissiveMap', 'aoMap']) {
            if (m[k] && m[k].dispose) m[k].dispose();
          }
          if (m.dispose) m.dispose();
        }
      });
    } catch (e) { /* 忽略 */ }
    try { pmrem.dispose(); } catch (e) { /* 忽略 */ }
    try { composer.dispose(); } catch (e) { /* 版本差异忽略 */ }
    try { renderer.dispose(); } catch (e) { /* 忽略 */ }
  }

  return { scene, camera, renderer, world, focusOn, resetCamera, frameAvatar, attachModel, pedestals, setPip, destroy };
}

// ─────────────────────────── 灯光 ───────────────────────────
function buildLights(scene, theme) {
  const p = theme.palette;
  scene.add(new THREE.HemisphereLight(p.fill, 0x0a0806, 0.4));
  scene.add(new THREE.AmbientLight(0xffffff, 0.05));

  // 空间主光：压出人物的体积与地面投影
  const key = new THREE.DirectionalLight(p.key, 0.95);
  key.position.set(-2.2, 5.0, 3.6);
  key.castShadow = true;
  key.shadow.mapSize.set(1024, 1024);
  key.shadow.camera.near = 1;
  key.shadow.camera.far = 16;
  key.shadow.camera.left = -5; key.shadow.camera.right = 5;
  key.shadow.camera.top = 5; key.shadow.camera.bottom = -2;
  key.shadow.bias = -0.0009;
  scene.add(key);

  // 人物面部主光（右前上方，制造鼻影与脸颊过渡）
  const face = new THREE.SpotLight(p.key, 7, 13, 0.46, 0.45, 1.6);
  face.position.set(1.6, 3.1, 3.6);
  face.target.position.set(0, 1.28, 1.2);
  face.castShadow = true;
  face.shadow.mapSize.set(1024, 1024);
  face.shadow.bias = -0.0007;
  face.shadow.camera.near = 1;
  face.shadow.camera.far = 10;
  scene.add(face, face.target);

  // 左前补光，抬暗部
  const fill = new THREE.SpotLight(p.accent2, 2.6, 12, 0.6, 0.9, 1.5);
  fill.position.set(-2.6, 2.2, 3.2);
  fill.target.position.set(0, 1.1, 1.2);
  scene.add(fill, fill.target);

  // 背后轮廓光，让人物从暗背景里“亮”出来
  const rim = new THREE.SpotLight(p.key, 5, 10, 0.62, 0.8, 1.5);
  rim.position.set(-1.6, 2.7, -0.9);
  rim.target.position.set(0, 1.35, 1.2);
  scene.add(rim, rim.target);

  return {
    each(ex, pos) {
      const s = new THREE.SpotLight(p.key, 7.2, 8, 0.42, 0.75, 1.8);
      s.position.set(pos[0] * 0.9 + 0.25, 3.6, pos[2] + 1.65);
      s.target.position.set(pos[0], 1.3, pos[2]);
      s.castShadow = false;
      scene.add(s, s.target);
    },
  };
}

// ─────────────────────────── 地面 ───────────────────────────
function buildFloor(world, theme) {
  const floor = mesh(
    new THREE.PlaneGeometry(60, 60),
    surf('stone', 22, 22, { color: 0x63594f, metalness: 0.2, roughness: 0.42, normalScale: 0.5 })
  );
  floor.rotation.x = -Math.PI / 2;
  floor.receiveShadow = true;
  world.add(floor);

  // 中央圆形铜线嵌条，撑住“展厅”的仪式感
  const inlay = new THREE.Mesh(
    new THREE.RingGeometry(2.05, 2.12, 96),
    new THREE.MeshStandardMaterial({ color: 0x9c7a42, metalness: 0.9, roughness: 0.32 })
  );
  inlay.rotation.x = -Math.PI / 2;
  inlay.position.set(0, 0.006, 1.15);
  world.add(inlay);

  const inlay2 = new THREE.Mesh(
    new THREE.RingGeometry(2.62, 2.66, 96),
    new THREE.MeshStandardMaterial({ color: 0x8a6c3c, metalness: 0.85, roughness: 0.38 })
  );
  inlay2.rotation.x = -Math.PI / 2;
  inlay2.position.set(0, 0.006, 1.15);
  world.add(inlay2);

  const glow = new THREE.Mesh(
    new THREE.CircleGeometry(2.1, 64),
    new THREE.MeshBasicMaterial({ color: theme.palette.accent, transparent: true, opacity: 0.045 })
  );
  glow.rotation.x = -Math.PI / 2;
  glow.position.set(0, 0.005, 1.15);
  world.add(glow);
}

// ─────────────────────────── 博物馆大厅 ───────────────────────────
function buildHall(world, theme) {
  const p = theme.palette;
  const wallMat = surf('plaster', 14, 5, { color: 0x6f5b45, roughness: 0.95, normalScale: 0.7 });
  const trimMat = surf('wood', 10, 1, { color: 0x74543a, roughness: 0.66 });
  const stoneMat = surf('stone', 3, 3, { color: 0x6d655a, metalness: 0.1, roughness: 0.72, normalScale: 0.6 });
  const darkTrim = new THREE.MeshStandardMaterial({ color: 0x35281c, metalness: 0.35, roughness: 0.6 });

  const BACK = -7.4;

  // 后墙
  const back = box(34, 9.2, 0.3, wallMat, 0, 4.6, BACK);
  back.receiveShadow = true;
  world.add(back);

  // 木质墙裙 + 踢脚线
  world.add(box(34, 1.15, 0.06, trimMat, 0, 0.575, BACK + 0.19));
  world.add(box(34, 0.1, 0.11, darkTrim, 0, 1.18, BACK + 0.22));
  world.add(box(34, 0.16, 0.1, darkTrim, 0, 0.08, BACK + 0.22));

  // 洗墙光：从墙裙向上渐隐的暖光，让大墙有被打过光的层次，而不是一片死黑
  const wcv = document.createElement('canvas');
  wcv.width = 8; wcv.height = 256;
  const wg = wcv.getContext('2d');
  const wgr = wg.createLinearGradient(0, 256, 0, 0);
  wgr.addColorStop(0, 'rgba(255,208,152,0.08)');
  wgr.addColorStop(0.12, 'rgba(255,206,150,0.34)');
  wgr.addColorStop(0.4, 'rgba(255,194,140,0.22)');
  wgr.addColorStop(0.78, 'rgba(255,180,124,0.04)');
  wgr.addColorStop(1, 'rgba(255,170,110,0)');
  wg.fillStyle = wgr;
  wg.fillRect(0, 0, 8, 256);
  const washTex = new THREE.CanvasTexture(wcv);
  washTex.colorSpace = THREE.SRGBColorSpace;
  const wash = new THREE.Mesh(
    new THREE.PlaneGeometry(34, 8.7),
    new THREE.MeshBasicMaterial({
      map: washTex, transparent: true, blending: THREE.AdditiveBlending, depthWrite: false,
    })
  );
  // 放在壁柱和墙裙前面，一整片墙都被“洗”到，墙裙不再是死黑一条
  wash.position.set(0, 4.45, BACK + 0.42);
  world.add(wash);

  // 壁柱与竖向灯带
  const stripMat = new THREE.MeshStandardMaterial({
    color: 0x1a1208, emissive: p.accent, emissiveIntensity: 0.38, roughness: 0.4,
  });
  for (const x of [-7.2, -3.9, 3.9, 7.2]) {
    world.add(box(0.52, 7.4, 0.26, stoneMat, x, 3.9, BACK + 0.24));
    world.add(box(0.66, 0.14, 0.34, stoneMat, x, 7.66, BACK + 0.26));
    if (x !== 0) {
      world.add(box(0.035, 4.6, 0.02, stripMat, x + 0.34, 4.4, BACK + 0.38));
    }
  }

  // 顶部檐口
  world.add(box(34, 0.22, 0.5, stoneMat, 0, 7.75, BACK + 0.32));

  // 两侧墙
  for (const s of [-1, 1]) {
    const side = box(0.3, 9.2, 15, wallMat, s * 9.2, 4.6, -0.4);
    side.receiveShadow = true;
    world.add(side);
    world.add(box(0.06, 1.15, 15, trimMat, s * 9.02, 0.575, -0.4));
  }

  // 顶棚与暗槽灯
  const ceil = box(34, 0.3, 16, new THREE.MeshStandardMaterial({ color: 0x1d1813, roughness: 0.95 }), 0, 8.55, -0.4);
  world.add(ceil);
  const coveMat = new THREE.MeshStandardMaterial({
    color: 0x1a1208, emissive: p.key, emissiveIntensity: 0.7, roughness: 0.5,
  });
  world.add(box(30, 0.06, 0.16, coveMat, 0, 8.32, -2.2));
  world.add(box(30, 0.06, 0.16, coveMat, 0, 8.32, 3.4));
  for (const s of [-1, 1]) world.add(box(0.16, 0.06, 13, coveMat, s * 7.4, 8.32, -0.4));

  // 近景两侧立柱（带柱础与柱头）
  for (const side of [-1, 1]) {
    for (let i = 0; i < 2; i++) {
      const z = 1.2 + i * 3.4;
      const col = cyl(0.2, 0.24, 6.4, 24, stoneMat, side * 5.4, 3.2, z);
      col.castShadow = true;
      world.add(col);
      world.add(cyl(0.3, 0.34, 0.16, 24, darkTrim, side * 5.4, 0.08, z));
      world.add(cyl(0.27, 0.22, 0.18, 24, stoneMat, side * 5.4, 6.42, z));
      world.add(box(0.44, 0.3, 0.44, stoneMat, side * 5.4, 6.6, z));
    }
  }

  // 顶部吊灯（自发光盘 + 实体灯具）
  const lampMat = new THREE.MeshStandardMaterial({
    color: 0x201810, emissive: 0xffd9a0, emissiveIntensity: 1.1, roughness: 0.5,
  });
  for (const x of [-3.8, 0, 3.8]) {
    world.add(cyl(0.02, 0.02, 0.5, 8, darkTrim, x, 8.1, -1.4));
    world.add(cyl(0.22, 0.1, 0.14, 20, darkTrim, x, 7.82, -1.4));
    world.add(cyl(0.2, 0.2, 0.03, 20, lampMat, x, 7.74, -1.4));
  }
}

// ─────────────────────────── 展台 ───────────────────────────
function buildPedestal(world, pos, theme, label, topR = 0.44) {
  const g = new THREE.Group();
  g.position.set(pos[0], 0, pos[2]);

  // 米色石灰岩展台：真实博物馆的展陈基座，比深色木台更亮、更有石材质感
  const stone = surf('stone', 3, 1, { color: 0xc0b3a0, roughness: 0.56, normalScale: 0.55 });
  const stone2 = surf('stone', 2, 1, { color: 0xa79a8c, roughness: 0.66, normalScale: 0.5 });
  const bodyR = topR - 0.10;

  const base = cyl(topR - 0.02, topR + 0.04, 0.10, 48, stone2, 0, 0.05, 0);
  base.receiveShadow = base.castShadow = true;
  g.add(base);

  // 台身直筒，顶端略有收分，像整块石料雕出来的基座
  const body = cyl(bodyR - 0.02, bodyR, 0.55, 48, stone, 0, 0.375, 0);
  body.castShadow = body.receiveShadow = true;
  g.add(body);

  // 台面压边：黄铜细线 + 石质台板
  const lip = cyl(bodyR + 0.012, bodyR + 0.012, 0.018, 48, MAT.brass(), 0, 0.645, 0);
  g.add(lip);
  const top = cyl(topR, topR, 0.06, 48, stone, 0, 0.68, 0);
  top.castShadow = true;
  top.receiveShadow = true;
  g.add(top);
  // 顶面浅槽，把展品稳稳托住
  const inset = cyl(topR - 0.07, topR - 0.07, 0.012, 48, stone2, 0, 0.716, 0);
  inset.receiveShadow = true;
  g.add(inset);

  const ring = mesh(
    new THREE.RingGeometry(topR + 0.022, topR + 0.08, 56),
    new THREE.MeshBasicMaterial({ color: theme.palette.accent, transparent: true, opacity: 0.22, side: THREE.DoubleSide }),
    0, 0.724, 0
  );
  ring.rotation.x = -Math.PI / 2;
  g.add(ring);

  // 展牌：随基座弧面贴合的曲面牌，比平牌更贴墙、也更像真实展陈
  if (label) {
    const plateTex = plateTexture(label, 'QIAOXIANG · JIANGMEN');
    // 牌面整体放大约 1/3：弧长与高度同比放大，贴图比例不变、文字不变形，
    // 但远景里每个字能多占一截像素（DEF-31）
    const plateW = THREE.MathUtils.clamp(bodyR * 2.35, 0.40, 0.56); // 弧长
    const plateH = plateW * 0.36;
    const theta = plateW / bodyR;                                  // 对应圆心角
    const plateGeo = new THREE.CylinderGeometry(
      bodyR + 0.006, bodyR + 0.006, plateH, 28, 1, true, -theta / 2, theta
    );
    const plate = mesh(plateGeo, new THREE.MeshStandardMaterial({
      map: plateTex,
      emissiveMap: plateTex,
      emissive: 0xffffff,
      // 发光贴图会把深褐刻字一起提亮，浅底深字的对比被冲淡；收低到 0.22（DEF-31）
      emissiveIntensity: 0.22,
      metalness: 0.02,
      roughness: 0.72,
      side: THREE.DoubleSide,
    }), 0, 0.40, 0);
    g.add(plate);

    // 上下两道铜压条，把牌子“装”进基座
    for (const dy of [-plateH / 2 - 0.004, plateH / 2 + 0.004]) {
      const bar = mesh(
        new THREE.CylinderGeometry(bodyR + 0.008, bodyR + 0.008, 0.008, 28, 1, true, -theta / 2 - 0.02, theta + 0.04),
        MAT.brass(), 0, 0.40 + dy, 0
      );
      g.add(bar);
    }
  }

  // 脚下一圈柔影，避免展台“浮”在反光地面上
  const sh = blobShadow();
  sh.rotation.x = -Math.PI / 2;
  sh.scale.set(1.6, 1.6, 1);
  sh.position.y = 0.003;
  g.add(sh);

  g.userData.ring = ring;
  g.userData.spin = 0.1;
  world.add(g);
  return g;
}

// ─────────────────────────── 展品 ───────────────────────────
function makeProp(type, palette, name) {
  const g = new THREE.Group();
  g.userData.scale = 1;
  switch (type) {
    // 开平碉楼：混凝土塔身 + 券窗 + 挑台 + 穹顶
    case 'diaolou': {
      const wall = surf('concrete', 2, 3, { color: 0xb5ad9e, roughness: 0.95, normalScale: 1.1 });
      const trim = surf('plaster', 2, 1, { color: 0xe4dccb, roughness: 0.9 });
      const tile = surf('roof', 2, 1, { color: 0x7a7268, roughness: 0.9 });
      const glass = warmGlass(0.7);
      const frame = surf('plaster', 1, 1, { color: 0xd9d0bd, roughness: 0.85 });
      const dark = new THREE.MeshStandardMaterial({ color: 0x15100b, roughness: 0.9 });

      g.add(box(0.62, 0.1, 0.62, trim, 0, 0.05, 0));
      g.add(box(0.56, 0.03, 0.56, wall, 0, 0.11, 0));

      let y = 0.12;
      const floors = 5;
      for (let i = 0; i < floors; i++) {
        const w = 0.52 - i * 0.022;
        const h = 0.2;
        const body = box(w, h, w, wall, 0, y + h / 2, 0);
        body.castShadow = body.receiveShadow = true;
        g.add(body);
        // 楼板挑出一圈线脚
        g.add(box(w + 0.06, 0.022, w + 0.06, trim, 0, y + h - 0.011, 0));

        const wy = y + h * 0.54;
        addWindow(g, 0.07, 0.1, -0.1, wy, w / 2 + 0.006, frame, glass, { sill: true });
        addWindow(g, 0.07, 0.1, 0.1, wy, w / 2 + 0.006, frame, glass, { sill: true });
        // 立面射击孔：碉楼的防卫特征，两侧各一道细缝
        g.add(box(0.012, 0.03, 0.014, dark, -w / 2 + 0.04, wy, w / 2 + 0.005));
        g.add(box(0.012, 0.03, 0.014, dark, w / 2 - 0.04, wy, w / 2 + 0.005));
        if (i >= 1) {
          addWindow(g, 0.07, 0.1, w / 2 + 0.006, wy, 0, frame, glass, { rotY: Math.PI / 2 });
          addWindow(g, 0.07, 0.1, -w / 2 - 0.006, wy, 0, frame, glass, { rotY: -Math.PI / 2 });
        }
        y += h;
      }

      // 底层铁门：厚铁板 + 门钉
      const ironMat = new THREE.MeshStandardMaterial({ color: 0x3b3833, metalness: 0.72, roughness: 0.42 });
      g.add(box(0.115, 0.15, 0.016, ironMat, 0, 0.12 + 0.075, 0.263));
      for (let r = 0; r < 3; r++) {
        for (let c = 0; c < 2; c++) {
          g.add(box(0.012, 0.012, 0.006, MAT.brass(), -0.022 + c * 0.044, 0.155 + r * 0.038, 0.272));
        }
      }

      // 顶层挑台（碉楼的“燕子窝”）
      const balY = y - 0.02;
      g.add(box(0.5, 0.026, 0.5, trim, 0, balY, 0));
      for (const s of [-1, 1]) {
        g.add(box(0.5, 0.07, 0.03, trim, 0, balY + 0.05, s * 0.235));
        g.add(box(0.03, 0.07, 0.5, trim, s * 0.235, balY + 0.05, 0));
      }
      for (let i = 0; i < 7; i++) {
        const x = -0.2 + i * 0.0666;
        g.add(box(0.012, 0.09, 0.012, trim, x, balY + 0.07, 0.24));
        g.add(box(0.012, 0.09, 0.012, trim, x, balY + 0.07, -0.24));
      }

      // 顶部小亭：券门 + 穹顶 + 金顶
      const pavY = balY + 0.1;
      g.add(box(0.3, 0.16, 0.3, wall, 0, pavY + 0.08, 0));
      addArch(g, 0.09, 0.09, 0, pavY + 0.005, 0.151, frame, dark);
      const sideArch = addArch(g, 0.09, 0.09, 0.151, pavY + 0.005, 0, frame, dark);
      sideArch.rotation.y = Math.PI / 2;
      g.add(box(0.34, 0.02, 0.34, trim, 0, pavY + 0.17, 0));
      const dome = mesh(new THREE.SphereGeometry(0.13, 20, 12, 0, Math.PI * 2, 0, Math.PI / 2), tile, 0, pavY + 0.18, 0);
      g.add(dome);
      g.add(cyl(0.016, 0.026, 0.13, 10, MAT.brass(), 0, pavY + 0.32, 0));
      g.add(mesh(new THREE.SphereGeometry(0.022, 12, 10), MAT.brass(), 0, pavY + 0.39, 0));
      g.userData.scale = 1.45;
      break;
    }

    // 赤坎古镇：三栋骑楼，真实券廊（暗部凹进 + 拱券 + 券柱）+ 木窗 + 女儿墙
    case 'oldtown': {
      const plaster = surf('plaster', 2, 2, { color: 0xd8cbb4, roughness: 0.92, normalScale: 0.9 });
      const plaster2 = surf('plaster', 2, 2, { color: 0xc4b394, roughness: 0.92, normalScale: 0.9 });
      const brick = surf('brick', 2, 2, { color: 0xb08a74, roughness: 0.95, normalScale: 1.1 });
      const roofTile = surf('roof', 3, 1, { color: 0x9aa0a6, roughness: 0.92 });
      const glass = warmGlass(0.55);
      const frame = surf('wood', 1, 1, { color: 0x51382a, roughness: 0.7, normalScale: 0.9 });
      const stone = surf('stone', 1, 1, { color: 0x9a9086, roughness: 0.85 });
      const dark = new THREE.MeshStandardMaterial({ color: 0x0d0906, roughness: 0.95 });

      // 街基底座
      g.add(box(1.04, 0.05, 0.36, stone, 0, 0.025, 0));

      const blocks = [
        { x: -0.34, w: 0.31, h: 0.62, mat: plaster, z: 0.02, sign: '银号' },
        { x: 0, w: 0.33, h: 0.82, mat: brick, z: 0.04, sign: '茶楼' },
        { x: 0.34, w: 0.31, h: 0.58, mat: plaster2, z: 0.0, sign: '当铺' },
      ];
      for (const b of blocks) {
        const y0 = 0.05;
        const T = 0.28;          // 楼体进深
        const oh = 0.26;         // 券廊净高
        const zf = b.z + T / 2;

        // 底层券廊：后墙凹进 + 券洞暗部
        g.add(box(b.w - 0.02, oh, 0.02, dark, b.x, y0 + oh / 2, b.z - T / 2 + 0.01));

        // 两侧墙垛
        for (const s of [-1, 1]) {
          g.add(box(0.036, oh, T, b.mat, b.x + s * (b.w / 2 - 0.018), y0 + oh / 2, zf));
        }

        // 拱券
        const inner = b.w - 0.072;
        const ar = inner / 2;
        const arch = mesh(new THREE.TorusGeometry(ar, 0.014, 6, 26, Math.PI), stone,
          b.x, y0 + oh - ar, zf + 0.004);
        g.add(arch);
        g.add(box(0.014, oh - ar, 0.02, stone, b.x - ar, y0 + (oh - ar) / 2, zf + 0.006));
        g.add(box(0.014, oh - ar, 0.02, stone, b.x + ar, y0 + (oh - ar) / 2, zf + 0.006));

        // 上层楼体
        const upH = b.h - oh;
        const upper = box(b.w, upH, T, b.mat, b.x, y0 + oh + upH / 2, zf);
        upper.castShadow = upper.receiveShadow = true;
        g.add(upper);

        // 二层木窗（带窗套与窗台）
        addWindow(g, b.w * 0.46, 0.14, b.x, y0 + oh + 0.13, zf + T / 2 + 0.004, frame, glass, { sill: true });
        if (b.h > 0.7) {
          addWindow(g, b.w * 0.4, 0.11, b.x, y0 + oh + 0.31, zf + T / 2 + 0.004, frame, glass, { sill: true });
        }

        // 檐口 + 女儿墙
        const py = y0 + b.h;
        g.add(box(b.w + 0.035, 0.03, T + 0.02, roofTile, b.x, py + 0.015, zf));
        g.add(box(b.w + 0.01, 0.075, 0.02, b.mat, b.x, py + 0.075, zf + T / 2 + 0.002));
        for (let i = 0; i < 4; i++) {
          const px = b.x - b.w * 0.35 + i * (b.w * 0.7 / 3);
          g.add(box(0.012, 0.052, 0.012, stone, px, py + 0.03, zf + T / 2 + 0.008));
        }
        // 立面壁柱（贯穿上下）
        g.add(box(0.018, b.h, 0.024, stone, b.x - b.w / 2 + 0.01, y0 + b.h / 2, zf + T / 2 + 0.004));
        g.add(box(0.018, b.h, 0.024, stone, b.x + b.w / 2 - 0.01, y0 + b.h / 2, zf + T / 2 + 0.004));

        // 店铺招牌：银号 / 茶楼 / 当铺，赤坎曾是「小广州」的商埠气象
        const signTex = shopSignTexture(b.sign);
        g.add(box(b.w * 0.7, 0.072, 0.012, new THREE.MeshStandardMaterial({
          map: signTex, emissiveMap: signTex, emissive: 0xffffff,
          emissiveIntensity: 0.24, roughness: 0.72, metalness: 0.05,
        }), b.x, py - 0.052, zf + T / 2 + 0.01));

        // 廊下竖招幌 + 挂灯
        g.add(box(0.05, 0.008, 0.008, MAT.brass(), b.x, y0 + oh - 0.024, zf - 0.02));
        g.add(box(0.028, 0.082, 0.005, new THREE.MeshStandardMaterial({ color: 0x9c2b21, roughness: 0.9 }), b.x, y0 + oh - 0.072, zf - 0.02));
        g.add(mesh(new THREE.SphereGeometry(0.017, 10, 8), MAT.ember(0xffb066), b.x + 0.095, y0 + oh - 0.05, zf - 0.055));
      }
      g.userData.scale = 1.26;
      break;
    }

    // 梁启超故居：青灰砖屋身 + 硬山瓦顶 + 院墙
    case 'residence': {
      const wall = surf('concrete', 2, 2, { color: 0xb9b3a7, roughness: 0.94, normalScale: 1.0 });
      const brick = surf('brick', 2, 1, { color: 0x9c8b7d, roughness: 0.95, normalScale: 1.1 });
      const tile = surf('roof', 3, 1, { color: 0x9aa0a6, roughness: 0.9 });
      const wood = surf('wood', 1, 1, { color: 0x4b3423, roughness: 0.7 });
      const stone = surf('stone', 1, 1, { color: 0x93897c, roughness: 0.85 });
      const dark = new THREE.MeshStandardMaterial({ color: 0x0e0a07, roughness: 0.95 });

      // 石阶与院基
      g.add(box(0.9, 0.05, 0.46, stone, 0, 0.025, 0.05));
      g.add(box(0.82, 0.03, 0.1, stone, 0, 0.065, 0.33));

      // 屋身
      const house = box(0.64, 0.4, 0.38, wall, 0, 0.25, -0.02);
      house.castShadow = house.receiveShadow = true;
      g.add(house);
      // 砖砌墙裙
      g.add(box(0.65, 0.1, 0.39, brick, 0, 0.11, -0.02));

      // 硬山顶：两片斜坡瓦面 + 正脊 + 檐口滴水
      for (const s of [-1, 1]) {
        const slope = box(0.4, 0.022, 0.46, tile, s * 0.165, 0.51, -0.02);
        slope.rotation.z = -s * 0.44;
        g.add(slope);
        g.add(box(0.4, 0.02, 0.03, tile, s * 0.165, 0.455, 0.215));
      }
      g.add(box(0.05, 0.045, 0.48, tile, 0, 0.585, -0.02));
      // 船形正脊：两端微微起翘，是岭南民居屋脊的样子
      for (const s of [-1, 1]) {
        const tip = box(0.05, 0.03, 0.09, tile, 0, 0.605, s * 0.255);
        tip.rotation.x = -s * 0.55;
        g.add(tip);
      }
      // 檐下阴影带，给立面做出进深
      g.add(box(0.66, 0.022, 0.024, dark, 0, 0.443, 0.178));

      // 正门：门框 + 木门 + 石阶
      addWindow(g, 0.15, 0.24, 0, 0.245, 0.192, stone, dark, { sill: true });
      const door = box(0.13, 0.21, 0.014, wood, 0, 0.245, 0.2);
      g.add(door);
      // 门环与门簪
      g.add(box(0.026, 0.026, 0.02, MAT.brass(), 0.035, 0.24, 0.21));
      g.add(box(0.026, 0.026, 0.02, MAT.brass(), -0.035, 0.24, 0.21));
      for (const s of [-1, 1]) g.add(box(0.012, 0.012, 0.03, MAT.brass(), s * 0.05, 0.375, 0.185));

      // 红对联 + 横批：一眼就能认出是岭南人家的门面
      const couplet = new THREE.MeshStandardMaterial({ color: 0xa8281c, roughness: 0.86 });
      for (const s of [-1, 1]) {
        g.add(box(0.022, 0.185, 0.005, couplet, s * 0.105, 0.245, 0.176));
      }
      g.add(box(0.20, 0.028, 0.005, couplet, 0, 0.368, 0.176));

      // 两侧花窗
      addWindow(g, 0.1, 0.12, -0.22, 0.29, 0.192, wood, warmGlass(0.45), { sill: true });
      addWindow(g, 0.1, 0.12, 0.22, 0.29, 0.192, wood, warmGlass(0.45), { sill: true });

      // 院墙与门楼
      g.add(box(0.34, 0.2, 0.05, wall, -0.28, 0.1, 0.26));
      g.add(box(0.34, 0.2, 0.05, wall, 0.28, 0.1, 0.26));
      g.add(box(0.07, 0.03, 0.07, tile, -0.28, 0.215, 0.26));
      g.add(box(0.07, 0.03, 0.07, tile, 0.28, 0.215, 0.26));
      g.userData.scale = 1.48;
      break;
    }

    // 新会陈皮：一碟三瓣开皮、向内卷曲的干果皮，新皮米黄、老皮深褐
    case 'chenpi': {
      const dryTex = chenpiDryTexture();
      const plateMat = new THREE.MeshStandardMaterial({ color: 0x211c18, roughness: 0.52, metalness: 0.06 });
      const innerMat = new THREE.MeshStandardMaterial({ color: 0x141110, roughness: 0.6 });

      // 四种年份的色调：从刚晒好的米黄，到陈化多年的深褐近黑
      const mats = [0xe3cea8, 0xbf9b6c, 0x7e5a35, 0x40281a].map((c) =>
        new THREE.MeshStandardMaterial({
          map: dryTex, color: c, roughness: 0.86, metalness: 0, side: THREE.DoubleSide,
        }));
      // 按比例取色：新皮多、老皮少，堆在一起才有深浅对比
      const pickMat = () => {
        const r = Math.random();
        return r < 0.34 ? mats[0] : r < 0.66 ? mats[1] : r < 0.9 ? mats[2] : mats[3];
      };

      // 木托：矮一点，远看是一碟干货，不是一口黑锅
      const riserMat = surf('wood', 2, 1, { color: 0x6a5138, roughness: 0.74, normalScale: 1.0 });
      g.add(cyl(0.212, 0.224, 0.046, 36, riserMat, 0, 0.023, 0));

      // 黑釉浅碟：口大、身浅、沿圆润
      const tray = new THREE.Group();
      tray.position.y = 0.046;
      g.add(tray);

      const plate = cyl(0.302, 0.294, 0.018, 48, plateMat, 0, 0.010, 0);
      plate.castShadow = plate.receiveShadow = true;
      tray.add(plate);
      tray.add(cyl(0.292, 0.292, 0.005, 48, innerMat, 0, 0.018, 0));
      const rim = mesh(new THREE.TorusGeometry(0.298, 0.010, 8, 56), plateMat, 0, 0.019, 0);
      rim.rotation.x = Math.PI / 2;
      tray.add(rim);

      // 铺满碟底的四层：整体低矮、彼此压叠，像随手摊开晾着的一碟陈皮
      const heap = [];
      for (let i = 0; i < 22; i++) {
        const a = (i / 22) * Math.PI * 2 + 0.35;
        const r = 0.08 + (i % 5) * 0.026;
        heap.push([Math.cos(a) * r, 0.021, Math.sin(a) * r]);
      }
      for (let i = 0; i < 15; i++) {
        const a = (i / 15) * Math.PI * 2 + 1.1;
        const r = 0.02 + (i % 4) * 0.036;
        heap.push([Math.cos(a) * r, 0.031, Math.sin(a) * r]);
      }
      for (let i = 0; i < 9; i++) {
        const a = (i / 9) * Math.PI * 2 + 0.6;
        const r = 0.012 + (i % 3) * 0.032;
        heap.push([Math.cos(a) * r, 0.041, Math.sin(a) * r]);
      }
      for (let i = 0; i < 5; i++) {
        const a = (i / 5) * Math.PI * 2 + 2.2;
        const r = 0.008 + (i % 2) * 0.024;
        heap.push([Math.cos(a) * r, 0.051, Math.sin(a) * r]);
      }

      let seed = 0.37;
      for (const [x, y, z] of heap) {
        seed += 1.43;
        const size = 0.036 + Math.random() * 0.024;
        // 卷边朝上，深浅不一；偶尔四瓣，形状更随机
        const geo = chenpiPeelGeo(size, 0.35 + Math.random() * 0.55,
          3 + (Math.random() < 0.3 ? 1 : 0), seed);
        const m = mesh(geo, pickMat(), x, y, z);
        m.rotation.set(
          (Math.random() - 0.5) * 0.42,
          Math.random() * Math.PI * 2,
          (Math.random() - 0.5) * 0.42
        );
        m.castShadow = true;
        tray.add(m);
      }
      g.userData.scale = 1.32;
      break;
    }

    // 侨批银信：斜面展架 + 手书信笺 + 银元
    case 'letter': {
      const stand = surf('wood', 1, 1, { color: 0x7a5433, roughness: 0.6, normalScale: 1.2 });
      const letterMat = new THREE.MeshStandardMaterial({
        map: tex('letter_color.jpg', 1, 1, true),
        color: 0xd9c9a8,
        roughness: 0.92,
        metalness: 0,
      });
      const silver = new THREE.MeshStandardMaterial({ color: 0xd8d6d0, metalness: 1, roughness: 0.28 });

      const tilt = -0.28;
      // 斜面展板（比信笺略大一圈，留出展陈边距）
      const board = box(0.6, 0.44, 0.026, stand, 0, 0.29, 0);
      board.rotation.x = tilt;
      board.castShadow = true;
      g.add(board);
      // 支架与底座
      g.add(box(0.62, 0.028, 0.3, stand, 0, 0.014, 0.02));
      for (const s of [-1, 1]) {
        g.add(box(0.028, 0.12, 0.028, stand, s * 0.24, 0.075, 0.12));
      }
      // 信笺贴在展板上
      const letter = mesh(new THREE.PlaneGeometry(0.52, 0.36), letterMat, 0, 0.3, 0.019);
      letter.rotation.x = tilt;
      g.add(letter);
      // 红色火漆印（立体小红点，远看也能认出是侨批）
      const seal = cyl(0.028, 0.028, 0.008, 20, new THREE.MeshStandardMaterial({ color: 0x8e241c, roughness: 0.45 }), 0.18, 0.19, 0.028);
      seal.rotation.x = tilt + Math.PI / 2;
      g.add(seal);

      // 信封：米色信封 + 红框 + 竖写「侨批」+ 邮票，是侨批最典型的样子
      const envelopeTex = (() => {
        const cv = document.createElement('canvas');
        cv.width = 1024; cv.height = 640;   // 2 倍采样，信封上的小字才撑得住（DEF-31）
        const c = cv.getContext('2d');
        c.fillStyle = '#efe3c6'; c.fillRect(0, 0, 1024, 640);
        for (let i = 0; i < 60; i++) {
          c.fillStyle = `rgba(150,124,84,${Math.random() * 0.06})`;
          c.fillRect(Math.random() * 1024, Math.random() * 640, 80, 6);
        }
        c.strokeStyle = '#a8332a'; c.lineWidth = 18;
        c.strokeRect(32, 32, 960, 576);
        c.fillStyle = '#8f2a22';
        c.font = '800 184px "KaiTi","STKaiti",serif';
        c.textAlign = 'center'; c.textBaseline = 'middle';
        c.fillText('侨', 712, 184);
        c.fillText('批', 712, 392);
        c.fillStyle = '#2e2011';
        c.font = '700 80px "KaiTi","STKaiti",serif';
        c.fillText('外付银', 300, 220);
        c.fillText('伍拾圆', 300, 336);
        c.fillStyle = '#9c2b23';
        c.fillRect(120, 464, 192, 112);
        c.fillStyle = '#f4ead4';
        c.font = '800 68px "KaiTi",serif';
        c.fillText('批局', 216, 522);
        return textTexture(cv);
      })();
      const envelope = box(0.30, 0.19, 0.008, new THREE.MeshStandardMaterial({
        map: envelopeTex, roughness: 0.9, metalness: 0,
      }), -0.13, 0.032, 0.07);
      envelope.rotation.set(-Math.PI / 2 + 0.06, 0.06, 0);
      envelope.castShadow = true;
      g.add(envelope);

      // 一叠待寄的侨批（略微错开，像真的摞在一起）
      for (let i = 0; i < 3; i++) {
        const stack = box(0.20, 0.006, 0.13, new THREE.MeshStandardMaterial({ color: 0xe3d6b8, roughness: 0.94 }), 0.20, 0.031 + i * 0.007, 0.10);
        stack.rotation.y = -0.12 + i * 0.07;
        g.add(stack);
      }
      // 捆信的麻绳
      g.add(box(0.012, 0.026, 0.14, new THREE.MeshStandardMaterial({ color: 0x8c7350, roughness: 0.95 }), 0.20, 0.043, 0.10));

      // 银元与铜钱放在底座上
      const coin = cyl(0.04, 0.04, 0.007, 28, silver, 0.29, 0.035, 0.14);
      g.add(coin);
      const coin2 = cyl(0.031, 0.031, 0.006, 24, MAT.brass(), 0.22, 0.032, 0.16);
      g.add(coin2);
      g.userData.scale = 1.55;
      break;
    }

    // 小鸟天堂：四百年古榕长成一座岛，一条水道从树冠中穿过，白鹭与夜鹭绕树归巢
    case 'banyan': {
      const bark = new THREE.MeshStandardMaterial({
        color: 0xdcc7a0, roughness: 0.95, metalness: 0,
        map: tex('wood_color.jpg', 1, 2, true),
        normalMap: tex('wood_normal.jpg', 1, 2), normalScale: new THREE.Vector2(1.7, 1.7),
      });
      const leafMat = new THREE.MeshStandardMaterial({
        map: leafCardTexture(), transparent: true, alphaTest: 0.42,
        side: THREE.DoubleSide, roughness: 0.84, metalness: 0,
      });
      const moss = new THREE.MeshStandardMaterial({ color: 0x1e2d16, roughness: 0.96 });
      const rootMat = new THREE.MeshStandardMaterial({ color: 0x5a4732, roughness: 0.94 });

      // 水面：中心亮、边缘暗，加几圈极淡波纹，水才有深浅
      const wcv = document.createElement('canvas');
      wcv.width = wcv.height = 256;
      const wg = wcv.getContext('2d');
      const wgrad = wg.createRadialGradient(128, 128, 6, 128, 128, 128);
      wgrad.addColorStop(0, '#5d776a');
      wgrad.addColorStop(0.55, '#43594e');
      wgrad.addColorStop(1, '#2b3b32');
      wg.fillStyle = wgrad;
      wg.fillRect(0, 0, 256, 256);
      wg.strokeStyle = 'rgba(232,244,232,0.10)';
      for (let i = 0; i < 9; i++) {
        wg.lineWidth = 0.7 + Math.random() * 1.1;
        wg.beginPath();
        wg.arc(128, 128, 22 + i * 13 + Math.random() * 5, 0, Math.PI * 2);
        wg.stroke();
      }
      const wtex = new THREE.CanvasTexture(wcv);
      wtex.colorSpace = THREE.SRGBColorSpace;
      const waterMat = new THREE.MeshStandardMaterial({ map: wtex, metalness: 0.88, roughness: 0.08 });

      // 环岛水面
      const pond = mesh(new THREE.CircleGeometry(0.335, 60), waterMat, 0, 0.012, 0);
      pond.rotation.x = -Math.PI / 2;
      g.add(pond);

      // 树影落在水面上的一圈暗色，避免水面像一块平整的油漆
      const refl = mesh(new THREE.CircleGeometry(0.238, 48),
        new THREE.MeshBasicMaterial({ color: 0x1d2b18, transparent: true, opacity: 0.38, depthWrite: false }),
        0, 0.0126, 0);
      refl.rotation.x = -Math.PI / 2;
      refl.scale.set(1, 1.62, 1);
      g.add(refl);

      // 岛：左右两块绿岸，中间让出一条可通船的水道
      const BANK_X = 0.158;
      const bankGeo = new THREE.SphereGeometry(1, 26, 16, 0, Math.PI * 2, 0, Math.PI / 2);
      for (const sx of [-1, 1]) {
        const bank = mesh(bankGeo, moss, sx * BANK_X, 0.012, -0.010);
        bank.scale.set(0.112, 0.098, 0.212);
        g.add(bank);
      }
      // 岸面高度：树干、气根要落在岸上，不能插在水里
      const bankTop = (x, z) => {
        let h = 0.012;
        for (const sx of [-1, 1]) {
          const dx = (x - sx * BANK_X) / 0.112;
          const dz = (z + 0.010) / 0.212;
          const rr = dx * dx + dz * dz;
          if (rr < 1) h = Math.max(h, 0.012 + 0.098 * Math.sqrt(1 - rr));
        }
        return h;
      };

      // 穿过树冠的蜿蜒水道：从岛前沿切进去，绕到岛后，游船就从观众眼前的这个缺口进出
      const chCurve = new THREE.CatmullRomCurve3([
        new THREE.Vector3(0.026, 0, 0.330),
        new THREE.Vector3(-0.006, 0, 0.250),
        new THREE.Vector3(0.018, 0, 0.150),
        new THREE.Vector3(-0.012, 0, 0.030),
        new THREE.Vector3(0.010, 0, -0.110),
        new THREE.Vector3(-0.004, 0, -0.230),
      ]);
      const CH_SEG = 44;
      const cpos = [], cuv = [], cidx = [];
      for (let i = 0; i <= CH_SEG; i++) {
        const t = i / CH_SEG;
        const p = chCurve.getPoint(t);
        const tan = chCurve.getTangent(t);
        const nx = -tan.z, nz = tan.x;
        const nl = Math.hypot(nx, nz) || 1;
        // 靠岛前沿张开成喇叭口，往里收窄，才看得出是一条钻进去的水道
        const half = (0.045 - 0.019 * t) * (0.92 + 0.08 * Math.sin(t * 6.1));
        cpos.push(p.x + (nx / nl) * half, 0.0132, p.z + (nz / nl) * half);
        cpos.push(p.x - (nx / nl) * half, 0.0132, p.z - (nz / nl) * half);
        cuv.push(0, t * 5, 1, t * 5);
        if (i < CH_SEG) {
          const a = i * 2;
          cidx.push(a, a + 1, a + 2, a + 1, a + 3, a + 2);
        }
      }
      const chGeo = new THREE.BufferGeometry();
      chGeo.setAttribute('position', new THREE.Float32BufferAttribute(cpos, 3));
      chGeo.setAttribute('uv', new THREE.Float32BufferAttribute(cuv, 2));
      chGeo.setIndex(cidx);
      chGeo.computeVertexNormals();
      g.add(mesh(chGeo, new THREE.MeshStandardMaterial({
        color: 0x4e6c5a, metalness: 0.72, roughness: 0.10, side: THREE.DoubleSide,
      })));

      // 岸边老树干：只落在岸上，而且让开水道中央，水面和游船前面保持通透
      for (let i = 0; i < 16; i++) {
        const a = (i / 16) * Math.PI * 2 + 0.2;
        const x = Math.cos(a) * (0.104 + (i % 3) * 0.018);
        const z = -0.010 + Math.sin(a) * (0.144 + (i % 4) * 0.014);
        const base = bankTop(x, z);
        if (base < 0.026) continue;
        // 老榕的干是扭的：粗细不一、各自外倾，高度压在树冠底下，不戳出冠面
        const h = 0.052 + (i % 4) * 0.012;
        const t = cyl(0.009 + (i % 3) * 0.002, 0.022 + (i % 4) * 0.004, h, 9, bark,
          x, base - 0.008 + h / 2, z);
        t.rotation.z = Math.cos(a) * (0.16 + (i % 3) * 0.06);
        t.rotation.x = Math.sin(a) * (0.10 + (i % 4) * 0.04);
        g.add(t);
      }
      // 两丛主树：各立在左右绿岸的中央，被树冠盖住
      for (const sx of [-1, 1]) {
        for (let i = 0; i < 3; i++) {
          const bx = sx * (0.158 + (i - 1) * 0.026);
          const bz = -0.010 + (i - 1) * 0.040;
          const base = bankTop(bx, bz);
          const h = 0.082 + i * 0.010;
          const t = cyl(0.018, 0.038, h, 11, bark, bx, base - 0.01 + h / 2, bz);
          t.rotation.z = sx * (0.10 + i * 0.04);
          t.rotation.x = (i - 1) * 0.08;
          g.add(t);
        }
      }

      // 树冠分三层：上层稀疏的树梢、中层浓密的主冠、下层垂到水面的裙摆
      const lobes = [
        { x: -0.162, y: 0.150, z: 0.030, rx: 0.106, ry: 0.078, rz: 0.128, dens: 1.00 },
        { x: -0.142, y: 0.144, z: -0.140, rx: 0.098, ry: 0.072, rz: 0.098, dens: 1.00 },
        { x: 0.162, y: 0.152, z: 0.020, rx: 0.106, ry: 0.078, rz: 0.128, dens: 1.00 },
        { x: 0.142, y: 0.146, z: -0.140, rx: 0.098, ry: 0.072, rz: 0.098, dens: 1.00 },
        { x: -0.192, y: 0.094, z: -0.030, rx: 0.086, ry: 0.040, rz: 0.112, dens: 0.55 },
        { x: 0.192, y: 0.094, z: -0.040, rx: 0.086, ry: 0.040, rz: 0.112, dens: 0.55 },
        { x: -0.096, y: 0.236, z: 0.030, rx: 0.068, ry: 0.046, rz: 0.072, dens: 0.46 },
        { x: 0.104, y: 0.244, z: -0.046, rx: 0.064, ry: 0.044, rz: 0.068, dens: 0.46 },
        { x: 0.004, y: 0.222, z: -0.198, rx: 0.078, ry: 0.048, rz: 0.060, dens: 0.60 },
        // 横跨水道的几团枝叶：中后段树冠连成一片，水道口留空，才能看见游船进去
        { x: 0.062, y: 0.208, z: -0.052, rx: 0.062, ry: 0.040, rz: 0.068, dens: 0.58 },
        { x: 0.000, y: 0.198, z: -0.158, rx: 0.072, ry: 0.044, rz: 0.060, dens: 0.58 },
      ];

      // 内芯：每个枝冠一颗近黑绿的实心，叶隙里看到的是树荫而不是球面
      const coreMat = new THREE.MeshStandardMaterial({ color: 0x16200e, roughness: 1, metalness: 0 });
      for (const L of lobes) {
        const core = mesh(new THREE.SphereGeometry(1, 16, 12), coreMat, L.x, L.y, L.z);
        core.scale.set(L.rx * 0.84, L.ry * 0.86, L.rz * 0.84);
        g.add(core);
      }

      // 叶片按枝冠体积分配：中层浓密、下层与上层稀疏
      const weightOf = (L) => L.rx * L.ry * L.dens;
      const wSum = lobes.reduce((s, L) => s + weightOf(L), 0);
      const counts = lobes.map((L) => Math.max(90, Math.round(11200 * weightOf(L) / wSum)));
      const leafTotal = counts.reduce((a, b) => a + b, 0);
      const inst = new THREE.InstancedMesh(crossLeafGeo(0.030, 0.040), leafMat, leafTotal);
      inst.castShadow = false;
      inst.receiveShadow = false;
      inst.userData.noShadow = true;

      const dummy = new THREE.Object3D();
      const col = new THREE.Color();
      const q = new THREE.Quaternion();
      const outward = new THREE.Vector3();
      const zAxis = new THREE.Vector3(0, 0, 1);
      const tiltQ = new THREE.Quaternion();
      const sunDir = new THREE.Vector3(0.76, 0.46, 0.55).normalize();
      let slot = 0;
      for (let li = 0; li < lobes.length; li++) {
        const L = lobes[li];
        for (let k = 0; k < counts[li]; k++) {
          const th = Math.random() * Math.PI * 2;
          const ph = Math.acos(1 - Math.random() * 1.34);
          const sp = Math.sin(ph), cp = Math.cos(ph);
          const puff = 0.94 + Math.random() * 0.14;
          const px = L.x + L.rx * puff * sp * Math.cos(th);
          const pz = L.z + L.rz * puff * sp * Math.sin(th);
          const yy = L.y + L.ry * puff * cp;
          dummy.position.set(px, Math.max(0.044, yy), pz);
          outward.set(
            (sp * Math.cos(th)) / L.rx, cp / L.ry, (sp * Math.sin(th)) / L.rz
          ).normalize();
          q.setFromUnitVectors(zAxis, outward);
          tiltQ.setFromEuler(new THREE.Euler(
            (Math.random() - 0.5) * 1.1,
            (Math.random() - 0.5) * 1.1,
            Math.random() * Math.PI * 2
          ));
          dummy.quaternion.copy(q).multiply(tiltQ);
          const s = 0.72 + Math.random() * 0.72;
          dummy.scale.set(s, s, s);
          dummy.updateMatrix();
          inst.setMatrixAt(slot, dummy.matrix);

          // 侧光模拟：右上方来的光照到冠面，冠底压暗，叶缝有明暗层次
          const relief = Math.min(1, Math.max(0, yy / 0.27));
          const sunLit = Math.max(0, outward.dot(sunDir));
          const lightF = 0.30 * relief + 0.70 * sunLit * (0.30 + 0.70 * relief);
          const lum = Math.min(0.58, 0.10 + 0.44 * lightF + Math.random() * 0.06);
          col.setHSL(0.246 + 0.014 * (1 - sunLit), 0.34 + 0.15 * (1 - sunLit), lum);
          inst.setColorAt(slot, col);
          slot++;
        }
      }
      inst.instanceMatrix.needsUpdate = true;
      if (inst.instanceColor) inst.instanceColor.needsUpdate = true;
      g.add(inst);

      // 气根：沿水道两侧和岛缘垂下，密密麻麻像一层帘子
      for (const side of [-1, 1]) {
        for (let i = 0; i < 10; i++) {
          const z = 0.118 - i * 0.032;
          const x = side * (0.050 + Math.random() * 0.010);
          const h = 0.075 + Math.random() * 0.055;
          g.add(cyl(0.0016, 0.0032, h, 6, rootMat, x, 0.072 - h / 2, z));
        }
      }
      for (let i = 0; i < 32; i++) {
        const a = (i / 32) * Math.PI * 2 + 0.08;
        const x = Math.cos(a) * 0.212;
        const z = -0.010 + Math.sin(a) * 0.206;
        const base = bankTop(x, z);
        if (base < 0.022 || Math.abs(x) < 0.052) continue;
        const h = 0.048 + (i % 5) * 0.012;
        g.add(cyl(0.0012, 0.0024, h, 6, rootMat, x, base - 0.004 + h / 2, z));
      }

      // —— 鸟：白鹭与夜鹭 ——
      // 身体是旋转体纺锤，颈与喙是渐细的连续曲面管，两端都埋进身体和头里，
      // 所以从任何角度看都是「一整只鸟」，不会出现头和身子分开的断口。
      const egretBody = new THREE.MeshStandardMaterial({ color: 0xfcfaf2, roughness: 0.58, metalness: 0 });
      const egretWing = new THREE.MeshStandardMaterial({ color: 0xe8ebe0, roughness: 0.66, metalness: 0, side: THREE.DoubleSide });
      const egretWingTip = new THREE.MeshStandardMaterial({ color: 0xd6dacd, roughness: 0.7, metalness: 0, side: THREE.DoubleSide });
      const heronBody = new THREE.MeshStandardMaterial({ color: 0x64707c, roughness: 0.6, metalness: 0 });
      const heronWing = new THREE.MeshStandardMaterial({ color: 0x3a4752, roughness: 0.64, metalness: 0, side: THREE.DoubleSide });
      const heronWingTip = new THREE.MeshStandardMaterial({ color: 0x2a343d, roughness: 0.68, metalness: 0, side: THREE.DoubleSide });
      const heronHead = new THREE.MeshStandardMaterial({ color: 0x1c242d, roughness: 0.56, metalness: 0 });
      const beakDark = new THREE.MeshStandardMaterial({ color: 0x24282d, roughness: 0.42 });
      const legBlack = new THREE.MeshStandardMaterial({ color: 0x22242a, roughness: 0.5 });
      const legDark = new THREE.MeshStandardMaterial({ color: 0x8d8a36, roughness: 0.54 });
      const toeYellow = new THREE.MeshStandardMaterial({ color: 0xd0a53c, roughness: 0.5 });
      const eyeRed = new THREE.MeshStandardMaterial({ color: 0xa32d1a, roughness: 0.38 });
      const eyeDark = new THREE.MeshStandardMaterial({ color: 0x14161a, roughness: 0.38 });

      // 纺锤形身体：绕 Z 轴的旋转体，前胸饱满、尾部收尖
      const bodyProfile = [
        [0.0000, -0.0300], [0.0028, -0.0272], [0.0062, -0.0230], [0.0092, -0.0175],
        [0.0116, -0.0105], [0.0128, -0.0020], [0.0129, 0.0048], [0.0116, 0.0105],
        [0.0094, 0.0148], [0.0066, 0.0176], [0.0042, 0.0192],
      ];
      const bodyGeo = new THREE.LatheGeometry(
        bodyProfile.map(([r, y]) => new THREE.Vector2(r, y)), 18);
      bodyGeo.rotateX(Math.PI / 2);
      // 拉长、收细：真实白鹭的身形是细长的纺锤，不是一颗球
      bodyGeo.scale(0.86, 0.88, 1.22);

      // 渐细管：按曲线生成一圈圈顶点，半径从 r0 线性收到 r1
      const taperedTube = (pts, r0, r1, mat, seg = 18, radial = 12) => {
        const curve = new THREE.CatmullRomCurve3(pts);
        const frames = curve.computeFrenetFrames(seg, false);
        const pos = [], idx = [];
        for (let i = 0; i <= seg; i++) {
          const t = i / seg;
          const p = curve.getPoint(t);
          const r = r0 + (r1 - r0) * t;
          const N = frames.normals[i], B = frames.binormals[i];
          for (let j = 0; j < radial; j++) {
            const a = (j / radial) * Math.PI * 2;
            const c = Math.cos(a), s = Math.sin(a);
            pos.push(
              p.x + r * (c * N.x + s * B.x),
              p.y + r * (c * N.y + s * B.y),
              p.z + r * (c * N.z + s * B.z)
            );
          }
        }
        for (let i = 0; i < seg; i++) {
          for (let j = 0; j < radial; j++) {
            const j2 = (j + 1) % radial;
            const a = i * radial + j, b = i * radial + j2;
            const c = (i + 1) * radial + j, d = (i + 1) * radial + j2;
            // 绕序要让法线朝外，否则管壁被背面剔除，看起来就是「断开的」
            idx.push(a, d, c, a, b, d);
          }
        }
        const geo = new THREE.BufferGeometry();
        geo.setAttribute('position', new THREE.Float32BufferAttribute(pos, 3));
        geo.setIndex(idx);
        geo.computeVertexNormals();
        return new THREE.Mesh(geo, mat);
      };

      // 两点之间的锥形骨（腿用）
      const rod = (a, b, r0, r1, mat, seg = 7) => {
        const dir = new THREE.Vector3().subVectors(b, a);
        const len = dir.length();
        const m = new THREE.Mesh(new THREE.CylinderGeometry(r1, r0, len, seg), mat);
        m.position.copy(a).addScaledVector(dir, 0.5);
        m.quaternion.setFromUnitVectors(new THREE.Vector3(0, 1, 0), dir.clone().normalize());
        return m;
      };

      // 翅膀轮廓：前缘略直、后缘带初级飞羽的缺口，是鸟翼真实的剪影
      // 用挤出而不是零厚度平面，翅膀才有厚度、边缘有倒角，不会像一张纸片
      const wingGeo = (span, chord, tipness) => {
        const s = new THREE.Shape();
        s.moveTo(0, chord * 0.52);
        s.bezierCurveTo(span * 0.34, chord * 0.86, span * 0.68, chord * 0.74, span, chord * 0.30);
        s.lineTo(span * (0.90 - tipness * 0.10), chord * 0.02);
        s.bezierCurveTo(span * 0.66, -chord * 0.64, span * 0.26, -chord * 0.86, 0, -chord * 0.50);
        s.closePath();
        const geo = new THREE.ExtrudeGeometry(s, {
          depth: 0.0018, bevelEnabled: true, bevelThickness: 0.0007,
          bevelSize: 0.0009, bevelSegments: 2, curveSegments: 12, steps: 1,
        });
        geo.rotateX(Math.PI / 2);
        geo.translate(0, 0.0009, 0);   // 厚度居中，翅膀中面在 y=0
        return geo;
      };

      // 折叠的翅改用贴合肋部的薄椭球，形状更耐看
      // 展开的飞行翼：一整片，外段上翘并后掠，根部插进胸口，中间不会断开
      const flyingWingGeo = () => {
        const span = 0.076, chord = 0.034;
        const s = new THREE.Shape();
        s.moveTo(0, chord * 0.46);
        s.bezierCurveTo(span * 0.30, chord * 0.82, span * 0.62, chord * 0.60, span, chord * 0.10);
        s.lineTo(span * 0.84, -chord * 0.14);
        s.bezierCurveTo(span * 0.56, -chord * 0.66, span * 0.22, -chord * 0.80, 0, -chord * 0.42);
        s.closePath();
        const geo = new THREE.ExtrudeGeometry(s, {
          depth: 0.0016, bevelEnabled: true, bevelThickness: 0.0006,
          bevelSize: 0.0008, bevelSegments: 1, curveSegments: 10, steps: 1,
        });
        geo.rotateX(Math.PI / 2);
        geo.translate(0, 0.0008, 0);
        // 离身体越远抬得越高（上反角）、越往后（后掠），翼尖就自然翘起来
        const pos = geo.attributes.position;
        for (let i = 0; i < pos.count; i++) {
          const t = Math.max(0, pos.getX(i)) / span;
          pos.setY(i, pos.getY(i) + t * t * 0.010);
          pos.setZ(i, pos.getZ(i) - t * t * 0.011);
        }
        geo.computeVertexNormals();
        return geo;
      };

      const buildBird = (kind, flying) => {
        const night = kind === 'night';
        const b = new THREE.Group();
        const bodyMat = night ? heronBody : egretBody;
        const wingMat2 = night ? heronWing : egretWing;
        const tipMat = night ? heronWingTip : egretWingTip;
        const headMat = night ? heronHead : egretBody;
        const beakMat = beakDark;
        const legMat = night ? legDark : legBlack;
        const toeMat = toeYellow;
        const k = night ? 1.10 : 1.0;

        const body = new THREE.Mesh(bodyGeo, bodyMat);
        body.scale.setScalar(k);
        b.add(body);

        // 颈：起点在胸腔里、终点在颅内，中间走 S 形
        let neckPts, headR;
        if (night) {
          // 夜鹭脖子粗短，但头要抬出背线，不能陷进身体里
          neckPts = [
            new THREE.Vector3(0, 0.0052, 0.0138),
            new THREE.Vector3(0, 0.0126, 0.0152),
            new THREE.Vector3(0, 0.0206, 0.0184),
            new THREE.Vector3(0, 0.0262, 0.0222),
          ];
          headR = 0.0086;
        } else if (flying) {
          neckPts = [
            new THREE.Vector3(0, 0.0040, 0.0150),
            new THREE.Vector3(0, 0.0054, 0.0230),
            new THREE.Vector3(0, 0.0058, 0.0306),
            new THREE.Vector3(0, 0.0056, 0.0362),
          ];
          headR = 0.0082;
        } else {
          // 站姿：长颈竖起来，头小、喙长，是白鹭最标志的剪影
          neckPts = [
            new THREE.Vector3(0, 0.0052, 0.0146),
            new THREE.Vector3(0, 0.0136, 0.0158),
            new THREE.Vector3(0, 0.0230, 0.0184),
            new THREE.Vector3(0, 0.0304, 0.0224),
          ];
          headR = 0.0074;
        }
        b.add(taperedTube(neckPts, 0.0058, 0.0036, bodyMat));

        const headPos = neckPts[3].clone().add(new THREE.Vector3(0, 0.0006, 0.0016));
        const head = mesh(new THREE.SphereGeometry(headR, 16, 12), headMat, headPos.x, headPos.y, headPos.z);
        head.scale.set(0.94, 0.92, 1.14);
        b.add(head);

        // 冠羽：夜鹭脑后两根细长的白翎，向后掠，不是头上的尖刺
        if (night) {
          for (const i of [-1, 1]) {
            const crest = taperedTube([
              new THREE.Vector3(i * 0.0022, headPos.y + 0.0044, headPos.z - 0.0020),
              new THREE.Vector3(i * 0.0034, headPos.y + 0.0052, headPos.z - 0.0092),
              new THREE.Vector3(i * 0.0038, headPos.y + 0.0034, headPos.z - 0.0150),
            ], 0.0012, 0.0003, headMat, 6, 5);
            b.add(crest);
          }
        }

        // 喙：从颅前伸出，根部埋进头里，末端略下弯
        const beakLen = night ? 0.0168 : 0.0206;
        b.add(taperedTube([
          new THREE.Vector3(0, headPos.y - 0.0012, headPos.z + headR * 0.42),
          new THREE.Vector3(0, headPos.y - 0.0020, headPos.z + headR * 0.42 + beakLen * 0.55),
          new THREE.Vector3(0, headPos.y - 0.0034, headPos.z + headR * 0.42 + beakLen),
        ], night ? 0.0034 : 0.0027, 0.0005, beakMat, 10, 8));

        // 眼
        for (const sx of [-1, 1]) {
          b.add(mesh(new THREE.SphereGeometry(0.0019, 8, 8), night ? eyeRed : eyeDark,
            sx * headR * 0.74, headPos.y + 0.0014, headPos.z + headR * 0.42));
        }

        // 腿脚：白鹭是黑腿黄脚，夜鹭是黄绿腿，都比身体细很多
        if (flying) {
          // 飞行时双腿向后并拢拖在尾后，细而暗，不抢画面
          for (const sx of [-1, 1]) {
            b.add(rod(
              new THREE.Vector3(sx * 0.0032, -0.0046, -0.0030),
              new THREE.Vector3(sx * 0.0036, -0.0038, -0.0400),
              0.0011, 0.0005, legMat, 5
            ));
          }
        } else {
          for (const sx of [-1, 1]) {
            b.add(rod(
              new THREE.Vector3(sx * 0.0040, -0.0062, 0.0044),
              new THREE.Vector3(sx * 0.0046, -0.0268, 0.0058),
              0.0012, 0.0008, legMat, 5
            ));
            for (const toe of [-1, 0, 1]) {
              b.add(rod(
                new THREE.Vector3(sx * 0.0046, -0.0270, 0.0058),
                new THREE.Vector3(sx * 0.0046 + toe * 0.0026, -0.0272, 0.0098),
                0.0005, 0.0002, toeMat, 4
              ));
            }
          }
        }

        // 翅膀：飞行的整片展开（根部埋进身体），停栖的折在身侧
        if (flying) {
          for (const sx of [-1, 1]) {
            const w = mesh(flyingWingGeo(), wingMat2, sx * 0.0040, 0.0022, 0.0058);
            w.scale.x = sx;
            b.add(w);
          }
        } else {
          // 站姿：翅收在身侧，是一块贴合肋部的薄翼，不是伸出去的板
          for (const sx of [-1, 1]) {
            const w = mesh(new THREE.SphereGeometry(1, 12, 10), wingMat2,
              sx * 0.0096, 0.0022, -0.0044);
            w.scale.set(0.0048, 0.0118, 0.0206);
            w.rotation.z = sx * 0.14;
            w.rotation.x = -0.05;
            b.add(w);
          }
        }

        // 尾羽：短而平，向后展开，末端略收
        const tail = mesh(wingGeo(0.015, 0.019, 0), wingMat2, 0, 0.0014, -0.0228);
        tail.rotation.y = Math.PI / 2;
        b.add(tail);

        b.traverse((m) => { m.userData.noShadow = true; });
        return b;
      };

      const putBird = (x, y, z, s, rotY, kind, flying) => {
        const b = buildBird(kind, flying);
        b.position.set(x, y, z);
        b.rotation.y = rotY;
        b.scale.setScalar(s);
        g.add(b);
      };

      // 停在树梢上的白鹭，和藏在冠内的夜鹭（坐标对齐枝冠顶面）
      const perch = [
        [-0.096, 0.282, 0.030, 'egret'], [0.104, 0.288, -0.046, 'egret'],
        [-0.162, 0.228, 0.030, 'egret'], [0.162, 0.230, 0.020, 'night'],
        [-0.142, 0.216, -0.140, 'night'], [0.142, 0.218, -0.142, 'egret'],
        [0.004, 0.270, -0.198, 'egret'], [-0.192, 0.134, -0.030, 'night'],
        [0.192, 0.134, -0.040, 'night'],
      ];
      for (let i = 0; i < perch.length; i++) {
        const [x, y, z, kind] = perch[i];
        // 头朝岛外，观众从正面能看到侧影和长喙；腿立在冠面上，不陷进叶子里
        putBird(x, y, z, 0.62 + Math.random() * 0.14,
          Math.atan2(x, z) + (Math.random() - 0.5) * 0.7, kind, false);
      }

      // 绕岛归巢的鸟群：白鹭为主，混着几只夜鹭
      for (let i = 0; i < 22; i++) {
        const a = (i / 22) * Math.PI * 2 * 1.7 + 0.42;
        const rr = 0.150 + (i % 7) * 0.030;
        const y = 0.300 + (i % 6) * 0.044 + Math.random() * 0.018;
        putBird(Math.cos(a) * rr, y, Math.sin(a) * rr,
          0.44 + Math.random() * 0.18,
          Math.atan2(-Math.sin(a), Math.cos(a)),
          i % 5 === 2 ? 'night' : 'egret', true);
      }

      // 水道口的观光画舫：停在喇叭口，观众一眼能看到船身和船舱
      const boat = new THREE.Group();
      boat.position.set(0.006, 0.016, 0.256);
      boat.rotation.y = 0.16;
      const hullMat = new THREE.MeshStandardMaterial({ color: 0x7c3b22, roughness: 0.68 });
      const trimMat = new THREE.MeshStandardMaterial({ color: 0xc2a166, roughness: 0.5, metalness: 0.35 });
      const cabinMat = new THREE.MeshStandardMaterial({ color: 0xe6dcc2, roughness: 0.82 });
      const roofMat = new THREE.MeshStandardMaterial({ color: 0x3f5c46, roughness: 0.7 });
      boat.add(box(0.040, 0.014, 0.096, hullMat, 0, 0, 0));
      boat.add(box(0.045, 0.004, 0.102, trimMat, 0, 0.008, 0));
      boat.add(box(0.034, 0.016, 0.054, cabinMat, 0, 0.020, -0.004));
      boat.add(box(0.043, 0.005, 0.064, roofMat, 0, 0.030, -0.004));
      for (const sx of [-1, 1]) {
        for (const sz of [-1, 1]) {
          boat.add(cyl(0.0012, 0.0012, 0.012, 5, trimMat, sx * 0.016, 0.020, -0.004 + sz * 0.026));
        }
      }
      g.add(boat);

      // 观鸟台：水边小木台 + 栏杆 + 望远镜
      const deck = new THREE.Group();
      deck.position.set(0.196, 0.014, 0.246);
      deck.rotation.y = -0.55;
      const plankMat = new THREE.MeshStandardMaterial({ color: 0x8a6a44, roughness: 0.86 });
      const railMat = new THREE.MeshStandardMaterial({ color: 0x6d5236, roughness: 0.8 });
      deck.add(box(0.092, 0.007, 0.058, plankMat, 0, 0, 0));
      for (const sx of [-1, 1]) {
        for (const sz of [-1, 1]) {
          deck.add(cyl(0.0022, 0.0022, 0.034, 6, railMat, sx * 0.042, -0.012, sz * 0.025));
        }
        deck.add(cyl(0.0022, 0.0022, 0.026, 6, railMat, sx * 0.042, 0.016, -0.026));
        deck.add(box(0.0040, 0.0030, 0.054, railMat, sx * 0.042, 0.029, 0.0));
      }
      deck.add(box(0.090, 0.0028, 0.0028, railMat, 0, 0.029, -0.026));
      // 望远镜
      const scope = new THREE.Group();
      scope.position.set(0.004, 0.020, 0.004);
      for (let i = 0; i < 3; i++) {
        const a = (i / 3) * Math.PI * 2;
        scope.add(cyl(0.0012, 0.0016, 0.020, 5, trimMat,
          Math.cos(a) * 0.005, -0.010, Math.sin(a) * 0.005));
      }
      const tube = cyl(0.0042, 0.0056, 0.026, 10, railMat, 0, 0.004, 0);
      tube.rotation.x = Math.PI / 2 - 0.22;
      scope.add(tube);
      deck.add(scope);
      g.add(deck);

      // 晨光光斑：阳光从右上穿过叶隙，落在冠顶和树脚上的一小片一小片亮斑
      const dappleCv = document.createElement('canvas');
      dappleCv.width = dappleCv.height = 128;
      const dg = dappleCv.getContext('2d');
      const dgrad = dg.createRadialGradient(64, 64, 2, 64, 64, 62);
      dgrad.addColorStop(0, 'rgba(255,243,214,0.85)');
      dgrad.addColorStop(0.45, 'rgba(255,236,198,0.34)');
      dgrad.addColorStop(1, 'rgba(255,230,184,0)');
      dg.fillStyle = dgrad;
      dg.fillRect(0, 0, 128, 128);
      const dappleTex = new THREE.CanvasTexture(dappleCv);
      dappleTex.colorSpace = THREE.SRGBColorSpace;
      const dappleMat = new THREE.MeshBasicMaterial({
        map: dappleTex, transparent: true, opacity: 0.42,
        blending: THREE.AdditiveBlending, depthWrite: false, side: THREE.DoubleSide,
      });
      // 冠顶受光处撒几片亮斑，树脚的水面也接一片漏光
      const dapples = [
        [0.078, 0.268, 0.070, 0.090], [-0.040, 0.282, -0.030, 0.075],
        [-0.146, 0.230, 0.052, 0.062], [0.150, 0.238, -0.062, 0.058],
        [0.010, 0.206, 0.060, 0.050], [-0.062, 0.222, -0.128, 0.046],
        [0.196, 0.048, 0.150, 0.070], [-0.182, 0.048, 0.118, 0.058],
      ];
      for (const [x, y, z, s] of dapples) {
        const d = mesh(new THREE.PlaneGeometry(s, s), dappleMat, x, y, z);
        d.rotation.x = -Math.PI / 2;
        d.userData.noShadow = true;
        g.add(d);
      }

      g.userData.scale = 1.72;
      break;
    }

    default: {
      g.add(mesh(new THREE.IcosahedronGeometry(0.24, 0), MAT.stone(), 0, 0.5, 0));
    }
  }

  // 叶片与小鸟这类小件不投阴影，省下阴影贴图开销
  g.traverse((o) => {
    if (o.isMesh && !o.userData.noShadow) { o.castShadow = true; o.receiveShadow = true; }
  });
  return g;
}

function buildDust(world, theme) {
  const n = 700;
  const geo = new THREE.BufferGeometry();
  const pos = new Float32Array(n * 3);
  for (let i = 0; i < n; i++) {
    pos[i * 3] = (Math.random() - 0.5) * 18;
    pos[i * 3 + 1] = Math.random() * 5.5 + 0.2;
    pos[i * 3 + 2] = (Math.random() - 0.5) * 14 - 1;
  }
  geo.setAttribute('position', new THREE.BufferAttribute(pos, 3));
  const mat = new THREE.PointsMaterial({
    color: theme.palette.accent, size: 0.022, transparent: true, opacity: 0.4,
    blending: THREE.AdditiveBlending, depthWrite: false,
  });
  const pts = new THREE.Points(geo, mat);
  world.add(pts);
  return pts;
}
