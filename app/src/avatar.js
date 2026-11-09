import * as THREE from 'three';
import { GLTFLoader } from 'three/addons/loaders/GLTFLoader.js';
import { VRMLoaderPlugin, VRMUtils } from '@pixiv/three-vrm';
import { VRMAnimationLoaderPlugin, createVRMAnimationClip } from '@pixiv/three-vrm-animation';

const EXPR_FALLBACK = { oh: 'ou', ou: 'oh', serious: 'relaxed', sad: 'relaxed' };

// 展品类型 → 讲解时的情绪/手势风格
const EXHIBIT_STYLE = {
  diaolou: { emotion: 'serious', gesture: 'point' },   // 开平碉楼
  chikan:  { emotion: 'happy',   gesture: 'hands' },   // 赤坎古镇
  liang:   { emotion: 'relaxed', gesture: 'hands' },   // 梁启超故居
  chenpi:  { emotion: 'happy',   gesture: 'spread' },  // 新会陈皮
  qiaopi:  { emotion: 'serious', gesture: 'hands' },   // 侨批银信
  bird:    { emotion: 'happy',   gesture: 'spread' },  // 小鸟天堂
};

export class Avatar {
  constructor(scene, camera, opts = {}) {
    this.scene = scene;
    this.camera = camera;
    this.vrm = null;
    this.t = 0;
    this.blinkIn = 2 + Math.random() * 2;
    this.blinkPhase = 0;
    this.gesture = 'idle';
    this.gestureT = 0;
    this.gestureDur = 1;
    this.emotion = 'neutral';
    this.vis = { aa: 0, ih: 0, ou: 0, ee: 0, oh: 0 };
    this.visTarget = { aa: 0, ih: 0, ou: 0, ee: 0, oh: 0 };
    this.lookTarget = new THREE.Object3D();
    this.lookTarget.position.set(0, 1.6, 3.5);
    scene.add(this.lookTarget);
    // 头部目标角度，插值过渡
    this.headYawTarget = 0;
    this.headPitchTarget = 0;
    // 讲解模式：目标展品位置（世界坐标）+ 展品 id（决定手势/情绪风格）
    this.explainTarget = null;
    this.exhibitId = null;
    this.speaking = false;
    this.lastVisAmp = 0;
    this.nodPhase = 0;           // 重音点头计数器
    this.weightShiftPhase = 0;   // 换重心节奏
    this.bodySwayAmp = 0;        // 身体微晃幅度，讲解时加大
    this.bodyYaw = 0;
    this.bodyYawTarget = 0;
    this.basePos = new THREE.Vector3(0, 0, 1.2);
    // VRMA 待机（第 1 层）：文件缺失或解析失败时保持 null，自动退回下面的程序化待机
    this.idleMixer = null;
    this.idleAction = null;
    this.fingerNodes = [];
    // 说话手势的平滑权重（第 2 层）：不是开关，是 0～1 的缓入缓出
    this.handAmp = 0;
    // 主题化（DEF-29）：展项→情绪/手势风格表允许由主题包注入（theme.exhibit_styles），
    // 未提供时退回内置默认表，换主题不再需要改代码
    this.exhibitStyle = (opts && opts.exhibitStyles) || EXHIBIT_STYLE;
    // 胸牌名字同样来自主题包，避免换景区后还挂着上一个讲解员的名字
    this.badgeName = (opts && opts.assistantName) || '';
  }

  async load(url) {
    const loader = new GLTFLoader();
    loader.register((parser) => new VRMLoaderPlugin(parser));
    const gltf = await loader.loadAsync(url);
    return this._setup(gltf);
  }

  // 已经拿到 ArrayBuffer 时用这个解析：可以和展品 GLB 并行下载，省首屏时间
  async loadBuffer(buffer) {
    const loader = new GLTFLoader();
    loader.register((parser) => new VRMLoaderPlugin(parser));
    const gltf = await loader.parseAsync(buffer, '');
    return this._setup(gltf);
  }

  // VRMA 待机：动画文件在 assets/models/idle.vrma。缺失/损坏时静默退回程序化待机
  async loadIdle(url) {
    if (!this.vrm) return false;
    try {
      const loader = new GLTFLoader();
      loader.register((parser) => new VRMAnimationLoaderPlugin(parser));
      const gltf = await loader.loadAsync(url);
      const anim = (gltf.userData.vrmAnimations || [])[0];
      if (!anim) throw new Error('VRMA 中没有动画数据');
      const clip = createVRMAnimationClip(anim, this.vrm);
      if (!clip || !clip.tracks.length) throw new Error('VRMA 无法重定向到当前模型');
      this.idleMixer = new THREE.AnimationMixer(this.vrm.scene);
      this.idleAction = this.idleMixer.clipAction(clip);
      this.idleAction.setLoop(THREE.LoopRepeat, Infinity);
      this.idleAction.play();
      console.info('[avatar] idle.vrma 接管待机，时长', clip.duration.toFixed(2), 's，轨道', clip.tracks.length);
      return true;
    } catch (e) {
      console.warn('[avatar] idle.vrma 未启用，退回程序化待机:', e.message || e);
      this.idleMixer = null;
      this.idleAction = null;
      return false;
    }
  }

  _setup(gltf) {
    const vrm = gltf.userData.vrm;
    if (!vrm) throw new Error('模型不是有效的 VRM 文件');
    try { VRMUtils.removeUnnecessaryVertices(gltf.scene); } catch (e) { console.warn('removeUnnecessaryVertices skipped', e); }
    try { VRMUtils.combineSkeletons(gltf.scene); } catch (e) { console.warn('combineSkeletons skipped', e); }
    vrm.scene.traverse((o) => { o.frustumCulled = false; o.castShadow = true; });
    this.vrm = vrm;
    this.scene.add(vrm.scene);

    // 不同来源的模型单位不一致，按包围盒统一到 1.62 米并让脚踩在地面上
    const box = new THREE.Box3().setFromObject(vrm.scene);
    const size = new THREE.Vector3();
    box.getSize(size);
    const scale = size.y > 0.1 ? 1.62 / size.y : 1;
    vrm.scene.scale.setScalar(scale);
    vrm.scene.position.set(this.basePos.x, -box.min.y * scale, this.basePos.z);
    vrm.scene.updateWorldMatrix(false, true);

    // bind pose 的包围盒未必等于蒙皮后的实际站位，按真实姿势的顶点重新量一次并落地
    vrm.update(0.016);
    const posed = new THREE.Box3().setFromObject(vrm.scene, true);
    const lift = -posed.min.y;
    if (Math.abs(lift) > 0.02) vrm.scene.position.y += lift;
    vrm.scene.updateWorldMatrix(false, true);
    console.info(
      '[avatar] bind', box.min.y.toFixed(2), box.max.y.toFixed(2),
      'posed', posed.min.y.toFixed(2), posed.max.y.toFixed(2),
      'lift', lift.toFixed(2), 'scale', scale.toFixed(3)
    );
    if (vrm.lookAt) vrm.lookAt.target = this.lookTarget;

    // 讲解员着装统一：米白上衣 + 深色下装，避免纯白 T 恤在展厅灯下过曝
    const WARDROBE = { Tops_01_CLOTH: 0xe4d8c0, Bottoms_01_CLOTH: 0x232a30, Shoes_01_CLOTH: 0x1a1a1d };
    vrm.scene.traverse((o) => {
      if (!o.isMesh || !o.material) return;
      const mats = Array.isArray(o.material) ? o.material : [o.material];
      for (const m of mats) {
        if (!m) continue;
        // 这个模型每个部件都带一份反向外壳描边材质（side=BackSide）。展厅顶光下
        // 它会变成一圈锯齿黑边，把颜色写入关掉即可干净地去掉描边、保留材质槽位
        if (/\(outline\)/i.test(m.name || '')) {
          m.colorWrite = false;
          m.depthWrite = false;
          continue;
        }
        if (m.outlineWidthMode) m.outlineWidthMode = 'none';
        if (WARDROBE[m.name] !== undefined) {
          m.color.setHex(WARDROBE[m.name]);
          m.needsUpdate = true;
        }
      }
    });

    // 讲解员胸牌：做成贴合胸口的曲面小牌，平贴片会在浅色衣服上显出「贴纸」感
    this.chestNode = vrm.humanoid.getNormalizedBoneNode('chest');
    if (this.chestNode) {
      // 4 倍采样 + 更重的字重 + 更高对比：胸牌在全身镜头里只有几十像素，
      // 256×176 的贴图连笔画都撑不住（DEF-31）
      const cv = document.createElement('canvas');
      cv.width = 1024; cv.height = 704;
      const ctx = cv.getContext('2d');
      ctx.fillStyle = '#081615';
      ctx.fillRect(0, 0, 1024, 704);
      ctx.strokeStyle = '#dcb268';
      ctx.lineWidth = 28;
      ctx.strokeRect(24, 24, 976, 656);
      ctx.fillStyle = '#ffebae';
      ctx.textAlign = 'center';
      ctx.textBaseline = 'middle';
      ctx.font = '800 216px "Microsoft YaHei","KaiTi",sans-serif';
      ctx.fillText('讲解员', 512, 262);
      ctx.font = '800 168px "Microsoft YaHei",sans-serif';
      // 主题化（DEF-29）：名字取自主题包 assistant.name，换景区不再挂错胸牌
      ctx.fillText(this.badgeName || '小 侨', 512, 522);
      const badgeTex = new THREE.CanvasTexture(cv);
      badgeTex.colorSpace = THREE.SRGBColorSpace;
      // 保留 mipmap 抗混叠：胸牌在全景只有二十来像素，关掉会变成锯齿；
      // 三线性 + 各向异性 16 让远景既平滑又尽量选到清晰层级（DEF-31 回归修正）
      badgeTex.generateMipmaps = true;
      badgeTex.minFilter = THREE.LinearMipmapLinearFilter;
      badgeTex.magFilter = THREE.LinearFilter;
      badgeTex.anisotropy = 16;

      const lanyard = new THREE.Group();

      // 弧面胸牌：半径略大于躯干，牌面贴在衣服表面而不陷进去。
      // 半径与高度同比放大 25%：屏幕像素不足时，物理放大比调贴图更直接
      const card = new THREE.Mesh(
        new THREE.CylinderGeometry(0.165, 0.165, 0.069, 22, 1, true, -0.31, 0.62),
        new THREE.MeshStandardMaterial({
          map: badgeTex, roughness: 0.55, metalness: 0.12, side: THREE.DoubleSide,
        })
      );
      lanyard.add(card);

      // 牌夹：顶端一小条金属卡扣（位置随牌面放大同步外移）
      const clip = new THREE.Mesh(
        new THREE.BoxGeometry(0.024, 0.006, 0.008),
        new THREE.MeshStandardMaterial({ color: 0xc8a45e, metalness: 0.9, roughness: 0.35 })
      );
      clip.position.set(0, 0.041, 0.160);
      lanyard.add(clip);

      this.lanyard = lanyard;
      this.badgeCard = card;
      vrm.scene.add(lanyard);
    }

    this.bones = {
      hips: vrm.humanoid.getNormalizedBoneNode('hips'),
      spine: vrm.humanoid.getNormalizedBoneNode('spine'),
      chest: vrm.humanoid.getNormalizedBoneNode('chest'),
      neck: vrm.humanoid.getNormalizedBoneNode('neck'),
      head: vrm.humanoid.getNormalizedBoneNode('head'),
      lUA: vrm.humanoid.getNormalizedBoneNode('leftUpperArm'),
      rUA: vrm.humanoid.getNormalizedBoneNode('rightUpperArm'),
      lLA: vrm.humanoid.getNormalizedBoneNode('leftLowerArm'),
      rLA: vrm.humanoid.getNormalizedBoneNode('rightLowerArm'),
      lHand: vrm.humanoid.getNormalizedBoneNode('leftHand'),
      rHand: vrm.humanoid.getNormalizedBoneNode('rightHand'),
      lUL: vrm.humanoid.getNormalizedBoneNode('leftUpperLeg'),
      rUL: vrm.humanoid.getNormalizedBoneNode('rightUpperLeg'),
    };
    // 手指近端关节：VRMA 与讲解手势都不写这几根，留给说话时的轻屈叠加
    for (const side of ['left', 'right']) {
      for (const f of ['Index', 'Middle', 'Ring', 'Little']) {
        const n = vrm.humanoid.getNormalizedBoneNode(side + f + 'Proximal');
        if (n) this.fingerNodes.push({ node: n, side });
      }
    }
    // 归一化骨骼的 hips 有固有高度，微动时要叠加在它上面，不能直接覆盖
    this.hipsY0 = this.bones.hips ? this.bones.hips.position.y : 0;
    this.expressions = {};
    // 把 serious/sad 一并扫描进可用表情表：VRM 自带这两个预设时直接用，
    // 没有时 _apply 会走 EXPR_FALLBACK（serious/sad → relaxed），不再永远失效（DEF-17）
    for (const n of ['aa', 'ih', 'ou', 'ee', 'oh', 'happy', 'blink', 'relaxed', 'serious', 'sad']) {
      this.expressions[n] = vrm.expressionManager.getExpression(n) ? n : null;
    }
    return vrm;
  }

  bounds() {
    const box = new THREE.Box3().setFromObject(this.vrm.scene, true);
    // 深度方向的包围盒对取景没有意义，压回到站位平面，避免机位被推远
    box.min.z = this.basePos.z;
    box.max.z = this.basePos.z;
    return box;
  }

  setGesture(name, duration = 2) {
    this.gesture = name;
    this.gestureT = 0;
    this.gestureDur = duration;
  }

  setEmotion(name) { this.emotion = name; }

  // 讲解模式：锁定目标展品，让身体/头部/手势都跟着走；情绪和手势风格由展品类型决定
  setExplainTarget(worldPos, exhibitId) {
    this.explainTarget = worldPos ? worldPos.clone() : null;
    this.exhibitId = exhibitId || null;
    if (this.explainTarget) {
      // 用实例风格表（可被主题包注入）而不是模块常量（DEF-29）
      const style = this.exhibitStyle[exhibitId];
      if (style) this.emotion = style.emotion;
    }
  }
  clearExplainTarget() {
    this.explainTarget = null;
    this.exhibitId = null;
  }

  setSpeaking(on) { this.speaking = !!on; }

  lookAtPoint(v) { this.lookTarget.position.copy(v); }

  // 口型：v 为 aa/ih/ou/ee/oh，w 强度
  speakViseme(v, w) {
    for (const k in this.visTarget) this.visTarget[k] = 0;
    this.visTarget[v] = Math.max(0.05, Math.min(1, w));
  }

  stopSpeaking() {
    for (const k in this.visTarget) this.visTarget[k] = 0;
  }

  _apply(name, value) {
    const key = this.expressions[name] || EXPR_FALLBACK[name];
    if (!key) return;
    if (value > 0.002) this.vrm.expressionManager.setValue(key, value);
    else this.vrm.expressionManager.setValue(key, 0);
  }

  update(dt) {
    if (!this.vrm) return;
    this.t += dt;
    const t = this.t;
    const b = this.bones;

    // 口型平滑，同时跟踪总音量用于讲解重音
    let amp = 0;
    for (const k in this.vis) {
      this.vis[k] += (this.visTarget[k] - this.vis[k]) * Math.min(1, dt * 14);
      this._apply(k, this.vis[k]);
      amp += this.vis[k];
    }
    amp = Math.min(1, amp * 0.8);
    this.lastVisAmp = this.lastVisAmp * 0.6 + amp * 0.4;

    // 眨眼
    this.blinkIn -= dt;
    if (this.blinkIn <= 0 && this.blinkPhase === 0) {
      this.blinkPhase = 1;
      // 讲话时眨眼稍慢一点
      this.blinkIn = (this.speaking ? 2.8 : 2.2) + Math.random() * 3.2;
    }
    if (this.blinkPhase > 0) {
      this.blinkPhase += dt / 0.16;
      const close = this.blinkPhase < 0.5 ? this.blinkPhase * 2 : (1 - this.blinkPhase) * 2;
      this._apply('blink', Math.max(0, close));
      if (this.blinkPhase >= 1) { this.blinkPhase = 0; this._apply('blink', 0); }
    }

    // ── 头部：根据 lookTarget 真正做 yaw/pitch，而不是只呼吸式微晃 ──
    if (b.head && this.lookTarget) {
      // lookTarget 在场景里，转到 vrm.scene 本地坐标
      const v = this.lookTarget.position.clone();
      this.vrm.scene.worldToLocal(v);
      const dx = v.x, dy = v.y - 1.55, dz = v.z;
      const dist = Math.max(0.1, Math.hypot(dx, dz));
      const targetYaw = Math.atan2(dx, dist);              // 水平偏转
      const targetPitch = Math.atan2(-dy, dist) * 0.7;     // 垂直偏转（头抬/低，幅度比水平小）
      // 人体头部活动范围有限，做安全钳制
      this.headYawTarget = Math.max(-0.55, Math.min(0.55, targetYaw));
      this.headPitchTarget = Math.max(-0.38, Math.min(0.38, targetPitch));
      // 叠加呼吸 + 微颤（活人感），讲解时颤得稍明显
      const live = this.speaking ? 0.018 : 0.01;
      const headY = Math.sin(t * 0.45) * 0.04 + (Math.random() - 0.5) * live;
      const headX = Math.sin(t * 0.9) * 0.02 + (Math.random() - 0.5) * live;
      // 平滑插值
      const k = Math.min(1, dt * 3.5);
      b.head.rotation.y += (this.headYawTarget + headY - b.head.rotation.y) * k;
      b.head.rotation.x += (this.headPitchTarget + headX - b.head.rotation.x) * k;
    }

    // ── 分层控制：VRMA 在跑时，躯干/大臂/腿归它，程序化只碰它不写的通道 ──
    // 讲解指向是作品的核心动作，此时暂停 VRMA 让位给 _updateArms；
    // 两边的姿态都由插值收敛，切换不会跳帧
    const vrmaIdle = !!(this.idleMixer && this.idleAction) && !this.explainTarget;
    if (vrmaIdle) {
      this.idleMixer.update(dt);
    } else {
      // 呼吸与站姿
      const breath = Math.sin(t * 1.1) * 0.012;
      // 身体微晃：讲解时幅度加大，让人物"活"起来
      const swayAmp = this.speaking ? 0.032 : 0.015;
      if (b.chest) {
        b.chest.rotation.x = breath * 0.7 + this._gestureChestX();
        b.chest.rotation.y = Math.sin(t * 0.5) * swayAmp;
      }
      if (b.hips) {
        b.hips.position.y = this.hipsY0 + Math.sin(t * 1.1) * 0.004;
        // 讲解时偶尔换重心——慢节奏左右晃
        if (this.speaking) {
          this.weightShiftPhase += dt * 0.35;
          b.hips.rotation.z = Math.sin(this.weightShiftPhase) * 0.022 + Math.sin(t * 0.5) * 0.008;
        } else {
          b.hips.rotation.z = Math.sin(t * 0.5) * 0.012;
        }
      }

      // 站姿：双脚略微分开、脚踝外撇
      if (b.lUL) b.lUL.rotation.z += (0.055 - b.lUL.rotation.z) * Math.min(1, dt * 2);
      if (b.rUL) b.rUL.rotation.z += (-0.055 - b.rUL.rotation.z) * Math.min(1, dt * 2);

      this._updateArms(dt);
    }

    // 身体朝向
    this.bodyYaw += (this.bodyYawTarget - this.bodyYaw) * Math.min(1, dt * 2.5);
    this.vrm.scene.rotation.y = this.bodyYaw;

    // 第 2 层：说话手势只写腕/指，VRMA 与讲解手势都不碰这些通道
    this._updateHands(dt, t);

    // 情绪（DEF-17）：每帧先清零全部情绪通道再设目标值，
    // 从 happy 切到 serious/sad 时不会残留上一情绪的表情
    const emoW = { happy: 0, relaxed: 0, serious: 0, sad: 0 };
    if (this.emotion === 'happy') emoW.happy = 0.5;
    else if (this.emotion === 'serious') emoW.serious = 0.45;
    else if (this.emotion === 'sad') emoW.sad = 0.4;
    else if (this.emotion === 'relaxed') { emoW.happy = 0.15; emoW.relaxed = 0.4; }
    for (const k in emoW) this._apply(k, emoW[k]);

    this.vrm.update(dt);

    // 胸牌跟随胸骨（只跟位移，朝向由整体转身决定，避免牌子自己乱转）
    if (this.lanyard && this.chestNode) {
      this.vrm.scene.updateMatrixWorld(true);
      const v = new THREE.Vector3();
      this.chestNode.getWorldPosition(v);
      this.vrm.scene.worldToLocal(v);
      this.lanyard.position.set(v.x, v.y - 0.05, v.z);
    }
  }

  _gestureChestX() {
    if (this.explainTarget || this.gesture === 'point') return -0.04;
    return 0;
  }

  _updateArms(dt) {
    const b = this.bones;
    if (!b.lUA) return;
    this.gestureT += dt;
    const k = Math.min(1, dt * 5);

    // 先决定"风格"：有讲解目标 → 用实例风格表（可被主题包注入）决定的手势；否则用服务端返回的
    let style = this.gesture;
    if (this.explainTarget) {
      const s = this.exhibitStyle[this.exhibitId];
      if (s) style = s.gesture;
      // 讲解期间手势不回落，持续
      this.gestureT = 0;
    }

    // 自然站姿：T 恤是宽松版型，手臂要张开一点才不会整条藏进衣服里
    let lz = -0.94, rz = 0.94, lx = 0.10, rx = 0.10, ly = 0.05, ry = -0.05;
    let lElbow = 0.2, rElbow = 0.2;

    // 讲解手势：根据展品位置动态选左手/右手 + 幅度
    if (this.explainTarget) {
      const v = this.explainTarget.clone();
      this.vrm.scene.worldToLocal(v);
      const side = v.x;     // 展品在角色左侧（负）还是右侧（正）
      const depth = v.z;    // 远近，越远手势越伸展
      const reach = Math.max(0, Math.min(1, depth / 5));

      // 重音时手臂有个小屈伸，像在打节拍
      const pulse = Math.sin(this.t * (6 + this.lastVisAmp * 4)) * (0.05 + this.lastVisAmp * 0.08);

      if (style === 'point') {
        // 单手前伸指展品
        const extend = 0.8 + reach * 0.15 + pulse;
        if (side >= 0) {
          // 展品在右侧 → 右手
          rz = 0.72 + pulse * 0.3;
          rx = extend;
          ry = -0.1 - side * 0.25;   // 指向展品的水平偏移
          rElbow = 0.25 - reach * 0.12;
          lz = -0.94;
        } else {
          lz = -0.72 - pulse * 0.3;
          lx = extend;
          ly = 0.1 - side * 0.25;
          lElbow = 0.25 - reach * 0.12;
          rz = 0.94;
        }
      } else if (style === 'spread') {
        // 双手展开在身体前，像在比展品有多大
        lz = -0.36 - reach * 0.2 - pulse * 0.3;
        rz = 0.36 + reach * 0.2 + pulse * 0.3;
        lx = 0.72 - reach * 0.1;
        rx = 0.72 - reach * 0.1;
        ly = 0.35 - side * 0.15;
        ry = -0.35 - side * 0.15;
        lElbow = 0.3 + pulse * 0.15;
        rElbow = 0.3 - pulse * 0.15;
      } else {
        // hands: 双手轻比在胸前，通用讲解（手臂微抬、肘弯曲、在身前小幅度比划）
        lz = -1.22 + pulse * 0.1;
        rz = 1.22 - pulse * 0.1;
        lx = 0.45 - reach * 0.08;
        rx = 0.45 - reach * 0.08;
        ly = 0.18;
        ry = -0.18;
        lElbow = 0.36 + pulse * 0.12;
        rElbow = 0.36 - pulse * 0.12;
      }
    } else if (this.gesture === 'wave') {
      const p = Math.min(1, this.gestureT / 0.6);
      rz = 1.15 - 1.35 * p;
      ry = 1.5;
      rx = -0.35;
      rElbow = 0.35 + Math.sin(this.gestureT * 7) * 0.28;
    } else if (this.gesture === 'point') {
      rz = 0.72; rx = 0.62; rElbow = 0.25; lz = -1.2;
    } else {
      const sway = Math.sin(this.t * 0.9) * 0.03;
      lz += sway; rz -= sway;
    }

    if (!this.explainTarget && this.gesture !== 'idle' && this.gestureT > this.gestureDur + 0.8) {
      this.gesture = 'idle';
    }

    b.lUA.rotation.z += (lz - b.lUA.rotation.z) * k;
    b.rUA.rotation.z += (rz - b.rUA.rotation.z) * k;
    b.lUA.rotation.x += (lx - b.lUA.rotation.x) * k;
    b.rUA.rotation.x += (rx - b.rUA.rotation.x) * k;
    b.lUA.rotation.y += (ly - b.lUA.rotation.y) * k;
    b.rUA.rotation.y += (ry - b.rUA.rotation.y) * k;
    if (b.lLA) b.lLA.rotation.x += (lElbow - b.lLA.rotation.x) * k;
    if (b.rLA) b.rLA.rotation.x += (rElbow - b.rLA.rotation.x) * k;
  }

  // 第 2 层：说话时的轻量手势。只写手腕和手指近端关节——这两组通道
  // VRMA 待机与讲解手势都不驱动，叠加上去不会抢骨骼。
  // 不说话时权重平滑衰减到 0，手不会僵在某个角度。
  _updateHands(dt, t) {
    const target = this.speaking ? 1 : 0;
    this.handAmp += (target - this.handAmp) * Math.min(1, dt * 8);
    const a = this.handAmp;
    const b = this.bones;
    if (b.rHand) {
      b.rHand.rotation.z = Math.sin(t * 4.0) * 0.12 * a;
      b.rHand.rotation.x = Math.sin(t * 2.3) * 0.05 * a;
    }
    if (b.lHand) {
      b.lHand.rotation.z = Math.sin(t * 4.0 + 1.5) * 0.08 * a;
      b.lHand.rotation.x = Math.sin(t * 2.7) * 0.04 * a;
    }
    // 手指轻屈，随说话音量略有起伏
    const curl = a * (0.03 + this.lastVisAmp * 0.06);
    for (const f of this.fingerNodes) {
      f.node.rotation.z = (f.side === 'left' ? 1 : -1) * curl;
    }
  }

  faceTowards(worldPos) {
    const dx = worldPos.x - this.basePos.x;
    const dz = worldPos.z - this.basePos.z;
    // 身体只微微转向展品（15-25°），真正的视线跟随交给头部，
    // 这样人物始终面向观众，不会侧过去"背对"讲解
    this.bodyYawTarget = Math.atan2(dx, dz) * 0.28;
  }

  resetFacing() { this.bodyYawTarget = 0; }
}
