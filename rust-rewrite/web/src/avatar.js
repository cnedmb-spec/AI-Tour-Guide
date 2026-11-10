import * as THREE from 'three';
import { GLTFLoader } from 'three/addons/loaders/GLTFLoader.js';
import { VRMLoaderPlugin, VRMUtils } from '@pixiv/three-vrm';
import { VRMAnimationLoaderPlugin, createVRMAnimationClip } from '@pixiv/three-vrm-animation';
import { explainGestureTargets, REST_POSE, POSE_LIBRARY } from './gesture.js';
import { MascotRig } from './mascot.js';

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
    this.gestureEnergy = 0;      // 当前手势笔画强度，供上身 / 头部协同
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
    // 吉祥物外观（可选）：null 时就是原来的 VRM 讲解员
    this.mascot = null;
    // 吉祥物模式下 VRM 只当动作骨架，人形网格从加载第一帧起就不绘制。
    // 旧实现等 47 MB 吉祥物解析完才在 MascotRig.bind() 里隐藏，加载层淡出时
    // 会先闪出旧人形讲解员；加载失败时由 showVrmMeshes() 恢复外观兜底。
    this.hideVrmOnLoad = !!(opts && opts.mascotMode);
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

  // 吉祥物外观：只换可见网格，不换骨架。加载失败时调用方应跳过，
  // 应用继续用原来的 VRM 讲解员，讲解 / 语音 / 问答链路完全不受影响。
  async loadMascot(buffer) {
    if (!this.vrm) return null;
    const gltf = await new GLTFLoader().parseAsync(buffer, '');
    const rig = new MascotRig(gltf, { scene: this.scene, basePos: this.basePos });
    rig.bind(this);
    this.mascot = rig;
    // VRM 胸牌挂在 vrm.scene 下、z≈0.16；吉祥物胸口在 z≈0.32，牌子会陷进模型里。
    // 吉祥物胸前自带纹章，暂时隐藏人形胸牌避免穿模。
    if (this.lanyard) this.lanyard.visible = false;
    console.info('[avatar] 吉祥物外观已接管，部位', Object.keys(rig.pivots).join(','));
    return rig;
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
    // bind pose 是 T-pose（双臂水平张开、手在肩高）。加载完成立刻把 6 个真实
    // 通道写成自然站姿，避免启动 / 换模型时闪过一下「张着手」。
    this._writeArm(this.bones.lUA, this.bones.lLA, -1, REST_POSE, 1);
    this._writeArm(this.bones.rUA, this.bones.rLA, 1, REST_POSE, 1);
    vrm.scene.updateMatrixWorld(true);

    // 归一化骨骼的 hips 有固有高度，微动时要叠加在它上面，不能直接覆盖
    this.hipsY0 = this.bones.hips ? this.bones.hips.position.y : 0;
    this.expressions = {};
    // 把 serious/sad 一并扫描进可用表情表：VRM 自带这两个预设时直接用，
    // 没有时 _apply 会走 EXPR_FALLBACK（serious/sad → relaxed），不再永远失效（DEF-17）
    for (const n of ['aa', 'ih', 'ou', 'ee', 'oh', 'happy', 'blink', 'relaxed', 'serious', 'sad']) {
      this.expressions[n] = vrm.expressionManager.getExpression(n) ? n : null;
    }
    // 与 scene.add(vrm.scene) 同一次同步调用内完成，首帧渲染之前人形网格就已不可见
    if (this.hideVrmOnLoad) this.hideVrmMeshes();
    return vrm;
  }

  // 隐藏 / 恢复 VRM 外观：只切网格可见性，骨架、spring bone、lookAt、表情全部保留。
  // 吉祥物模式首帧就隐藏；吉祥物加载失败时恢复，保证现场不会一个讲解员都没有。
  setVrmMeshesVisible(on) {
    if (!this.vrm) return;
    this.vrm.scene.traverse((o) => {
      if (o.isMesh || o.isSkinnedMesh) o.visible = on;
    });
    if (this.lanyard) this.lanyard.visible = on;
  }
  hideVrmMeshes() { this.setVrmMeshesVisible(false); }
  showVrmMeshes() { this.setVrmMeshesVisible(true); }

  bounds() {
    const box = new THREE.Box3().setFromObject(this.vrm.scene, true);
    // 吉祥物比人形 VRM 宽得多（Q 版大头 + 两侧耳机），全景取景必须把它算进去，
    // 否则 frameAvatar() 会按人形宽度定机位，把两侧手臂裁掉。
    if (this.mascot) box.union(this.mascot.bounds());
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
    // 每件展品从手势循环的第 0 相位开始，切换展项时不会从半途姿势突然跳过去
    this.gestureT = 0;
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

  setSpeaking(on) {
    this.speaking = !!on;
    // 每段讲解从时间线开头「抬手」起势，不在半途接上一个姿势
    if (on) this.gestureT = 0;
  }

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
      // 同步画面是正对人物的特写，头部大幅偏转在这里会变成歪头侧脸
      // （051410 录屏 30–31s 可见）；讲解员以正脸示人，只留小幅视线变化。
      this.headYawTarget = Math.max(-0.20, Math.min(0.20, targetYaw));
      this.headPitchTarget = Math.max(-0.16, Math.min(0.16, targetPitch));
      // 叠加呼吸 + 微颤（活人感），讲解时颤得稍明显
      const live = this.speaking ? 0.018 : 0.01;
      // 讲解时随语句轻点头：手臂动作收敛后，「在讲解」的观感主要由头部承担
      const nod = this.speaking ? Math.sin(t * 2.1) * 0.022 * (0.4 + this.lastVisAmp) : 0;
      const headY = Math.sin(t * 0.45) * 0.04 + (Math.random() - 0.5) * live;
      const headX = Math.sin(t * 0.9) * 0.02 + nod + (Math.random() - 0.5) * live;
      // 平滑插值
      const k = Math.min(1, dt * 3.5);
      b.head.rotation.y += (this.headYawTarget + headY - b.head.rotation.y) * k;
      b.head.rotation.x += (this.headPitchTarget + headX - b.head.rotation.x) * k;
    }

    // 分层控制：VRMA 只在纯待机时接管；讲解/说话都交给程序化手势，
    // 否则 VRMA 的静止姿势会把双臂固定在张开位（看起来像 T-pose 僵尸）。
    const vrmaIdle = !!(this.idleMixer && this.idleAction) && !this.explainTarget && !this.speaking;
    if (vrmaIdle) {
      this.idleMixer.update(dt);
      // VRMA 待机帧把双臂固定在张开位（录屏全景帧可见），叠加一层贴身站姿
      this._softenIdleArms();
    } else {
      // 呼吸与站姿
      const breath = Math.sin(t * 1.1) * 0.012;
      // 身体微晃：讲解时幅度加大，让人物"活"起来
      const swayAmp = this.speaking ? 0.032 : 0.015;
      if (b.chest) {
        const lean = this.speaking ? Math.sin(t * 1.3) * 0.012 : 0;
        // 抬手时上身跟着微微前倾、重心侧移，真人不会只动手臂；
        // gestureEnergy 来自笔画包络（上一帧值，差 16ms 无感），幅度刻意很小。
        const ge = this.gestureEnergy;
        b.chest.rotation.x = breath * 0.7 + this._gestureChestX() + lean + ge * 0.012;
        b.chest.rotation.y = Math.sin(t * 0.5) * swayAmp
          + (this.explainTarget ? Math.sin(this.gestureT * 0.7) * 0.018 : 0)
          + ge * 0.010 * Math.sin(this.gestureT * 0.8);
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

    // 吉祥物在 vrm.update 之后取骨骼世界旋转：此时 spring bone / lookAt /
    // VRMA / 讲解手势都已写进骨骼，复制过去才是最终姿态。
    if (this.mascot) this.mascot.sync(this);

    // 胸牌跟随胸骨（只跟位移，朝向由整体转身决定，避免牌子自己乱转）
    if (this.lanyard && this.chestNode) {
      this.vrm.scene.updateMatrixWorld(true);
      const v = new THREE.Vector3();
      this.chestNode.getWorldPosition(v);
      this.vrm.scene.worldToLocal(v);
      this.lanyard.position.set(v.x, v.y - 0.05, v.z);
    }
  }

  // VRMA 待机只负责躯干/呼吸；手臂改由这里接管。
  // mixer 每帧会重写大臂通道，弱插值会被覆盖掉，所以直接写目标值。
  // 与讲解态共用 REST 站姿，切换时不会突然跳成另一个姿势。
  _softenIdleArms() {
    const b = this.bones;
    if (!b.lUA) return;
    const sway = Math.sin(this.t * 0.62) * 0.035 + Math.sin(this.t * 0.23) * 0.015;
    const p = { ...REST_POSE, z: REST_POSE.z + sway };
    this._writeArm(b.lUA, b.lLA, -1, p);
    this._writeArm(b.rUA, b.rLA, 1, p);
  }

  // 把语义姿势写成归一化骨骼的 6 个真实通道。
  // 左右镜像：uy / z / efwd / inward 按侧别取反，ux / twist 两侧同号。
  // 定量扫描见 .diag/axis_scan.html：LA.rotation.x 只扭转不改手位，
  // 真正抬手靠 UA.rotation.x 负值 + LA.rotation.z / .y。
  _writeArm(ua, la, s, p, k = 1) {
    if (!ua) return;
    const tx = p.ux, ty = s * p.uy, tz = s * p.z;
    ua.rotation.x += (tx - ua.rotation.x) * k;
    ua.rotation.y += (ty - ua.rotation.y) * k;
    ua.rotation.z += (tz - ua.rotation.z) * k;
    if (la) {
      la.rotation.x += (p.twist - la.rotation.x) * k;
      la.rotation.y += (s * p.efwd - la.rotation.y) * k;
      la.rotation.z += (s * p.inward - la.rotation.z) * k;
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
    // 笔画曲线本身已经带缓入缓出与过冲，骨骼只需轻度低通；跟随太慢会把动作糊掉。
    const k = Math.min(1, dt * 7.0);

    let style = this.gesture;
    if (this.explainTarget) {
      const s = this.exhibitStyle[this.exhibitId];
      if (s) style = s.gesture;
    }

    let L = { ...REST_POSE }, R = { ...REST_POSE };
    let energy = 0;
    if (this.explainTarget) {
      const v = this.explainTarget.clone();
      this.vrm.scene.worldToLocal(v);
      const side = Math.max(-1, Math.min(1, v.x / 2));   // 展品在左 / 右 → 主手势手
      const g = explainGestureTargets(style, side, this.gestureT, this.t, this.lastVisAmp);
      L = g.left; R = g.right; energy = g.energy;
    } else if (this.speaking || this.gesture === 'wave' || this.gesture === 'point') {
      // 欢迎 / 闲聊 / 兜底回答没有展品焦点，但同样走连续笔画流，
      // 避免「只有展项特写才动手」和「摆好定住」。
      const g = explainGestureTargets('hands', 0, this.gestureT, this.t, this.lastVisAmp);
      L = g.left; R = g.right; energy = g.energy;
      if (this.gesture === 'wave') {
        // 只在连续流上再抬一点点，不整体替换成固定姿势（否则会突然跳一下）
        const p = Math.min(1, this.gestureT / 0.5);
        R = { ...R, ux: R.ux - p * 0.15 };
      } else if (this.gesture === 'point') {
        R = { ...R, inward: R.inward - 0.10, efwd: R.efwd + 0.08 };
      }
    } else {
      // 待机：REST 站姿 + 呼吸式摆臂
      const sway = Math.sin(this.t * 0.62) * 0.035 + Math.sin(this.t * 0.23) * 0.015;
      L = { ...REST_POSE, z: REST_POSE.z + sway };
      R = { ...REST_POSE, z: REST_POSE.z + sway };
    }

    if (!this.explainTarget && this.gesture !== 'idle' && this.gestureT > this.gestureDur + 0.8) {
      this.gesture = 'idle';
    }

    // 笔画强度供上身 / 头部做小幅协同；平滑一下避免切态时突变
    this.gestureEnergy += (energy - this.gestureEnergy) * Math.min(1, dt * 6);

    // 写入 6 个真实骨骼通道（含此前从未驱动过的 LA.rotation.y / .z）
    this._writeArm(b.lUA, b.lLA, -1, L, k);
    this._writeArm(b.rUA, b.rLA, 1, R, k);
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
      b.rHand.rotation.z = Math.sin(t * 3.1) * 0.10 * a;
      b.rHand.rotation.x = Math.sin(t * 2.3) * 0.05 * a;
    }
    if (b.lHand) {
      b.lHand.rotation.z = Math.sin(t * 3.1 + 1.5) * 0.08 * a;
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
    // 展品特写时主镜头看不到中央人物，同步画面是唯一可见画面，
    // 因此身体几乎不转向展品（原 0.28 叠加头部偏转会出现明显侧脸）。
    // 只留极小的姿态暗示，保持正脸对着观众。
    this.bodyYawTarget = Math.atan2(dx, dz) * 0.08;
  }

  resetFacing() { this.bodyYawTarget = 0; }
}
