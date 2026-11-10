import * as THREE from 'three';

// 吉祥物讲解员：把用户提供的单网格静态 GLB 接到现有 VRM 骨架上。
//
// 背景：该 GLB 是无骨骼、无动画、无表情的 Q 版熊猫（7476 个彼此独立的刚性块），
// 直接替换 avatar.vrm 会丢掉全部手势、口型、待机与视线。因此不换骨架，只换外观：
//   tools/build_mascot.py 先把模型按连通块拆成 6 个刚性部位（每个部位顶点已减去关节枢轴），
//   这里为每个部位建一个 pivot，每帧从 VRM 归一化骨骼读取「相对初始站姿的世界旋转增量」，
//   按部位缩放幅度后写到 pivot 上。VRM 继续跑 gesture.js / VRMA / 表情 / 视线，
//   只是它的网格被隐藏，动作被复制到吉祥物。
//
// pivot 全部平铺在 root 下而不是互相嵌套：骨骼的世界旋转增量本身已包含父链
// （head 的世界四元数含 chest 旋转），平铺可避免父级旋转被重复叠加。

const PART_BONE = {
  head:  'head',
  torso: 'chest',
  armL:  'lUA',
  armR:  'rUA',
  legL:  'lUL',
  legR:  'rUL',
};

// 头/躯干/腿用四元数增量的插值权重（0=完全不动，1=完全跟随）。
const GAIN = { head: 0.55, torso: 0.30, legL: 0.18, legR: 0.18 };

// 手臂必须做轴重映射，不能直接复制四元数：
// VRM 的讲解手势主要写 ua.rotation.x（大臂前后摆，实测跨度 1.38 rad），
// 从正面看几乎只是手的透视缩放——20 帧时间序列里"跟没动一样"。
// 改成把「前摆量」映射成绕 Z 的抬手，短手才会在屏幕上明显抬起 / 放下。
// 0.62 是旧版（耳机被误判成手）实拍定的幅度。重归类后随动的是真正的小白手，
// 相对袖口枢轴只有 ~0.10 m 臂长，0.62 在 420×525 小窗里位移偏小；
// 提到 0.85 后抬手约 20 px，仍不会插进躯干或出画。
const ARM_LIFT = 0.85;

const IDENTITY = new THREE.Quaternion();

export class MascotRig {
  /** @param gltf 已由 GLTFLoader 解析的 mascot.glb */
  constructor(gltf, { scene, basePos, boundsHeight = 1.088 }) {
    this.scene = scene;
    this.scale = 1.62 / boundsHeight;
    this.root = new THREE.Group();
    this.root.name = 'mascot';
    this.root.scale.setScalar(this.scale);
    this.root.position.set(basePos.x, 0, basePos.z);
    this.pivots = {};
    this.q0 = {};
    this.rest = {};
    this.restLA = null;
    this.restRA = null;
    this.hiddenVrmMeshes = [];
    this.ready = false;

    const q = new THREE.Quaternion();
    for (const node of gltf.scene.children) {
      const name = node.name;
      if (!PART_BONE[name] || !node.isMesh && !node.getObjectByProperty('isMesh', true)) continue;
      const pivot = new THREE.Group();
      pivot.name = 'mascot_' + name;
      pivot.position.copy(node.position);   // 关节枢轴（原始单位）

      // 一个部位可能含多个 mesh，逐个搬到 pivot 下；顶点已是相对枢轴的局部坐标
      const meshes = [];
      node.traverse((o) => { if (o.isMesh) meshes.push(o); });
      for (const src of meshes) {
        const mesh = new THREE.Mesh(src.geometry, src.material);
        mesh.frustumCulled = false;
        mesh.castShadow = true;
        mesh.receiveShadow = true;
        pivot.add(mesh);
      }
      this.root.add(pivot);
      this.pivots[name] = pivot;
    }
    scene.add(this.root);
    this.q = q;
    this._tmpQ = new THREE.Quaternion();
    this._tmpQ2 = new THREE.Quaternion();
    this._invRoot = new THREE.Quaternion();
  }

  /** 记录 VRM 自然站姿下每根骨骼的世界四元数，作为增量基准 */
  bind(avatar) {
    if (this.ready) return;
    avatar.vrm.scene.updateMatrixWorld(true);
    for (const [part, boneKey] of Object.entries(PART_BONE)) {
      const bone = avatar.bones[boneKey];
      if (!bone) continue;
      bone.getWorldQuaternion(this._tmpQ);
      this.q0[part] = this._tmpQ.clone();
      this.rest[part] = bone.rotation.clone();
    }
    if (avatar.bones.lLA) this.restLA = avatar.bones.lLA.rotation.clone();
    if (avatar.bones.rLA) this.restRA = avatar.bones.rLA.rotation.clone();
    // 隐藏 VRM 自己的网格（骨架、spring bone、lookAt、expression 全部保留）
    const badge = new Set();
    if (avatar.lanyard) avatar.lanyard.traverse((o) => badge.add(o));
    avatar.vrm.scene.traverse((o) => {
      if ((o.isMesh || o.isSkinnedMesh) && !badge.has(o)) {
        o.visible = false;
        this.hiddenVrmMeshes.push(o);
      }
    });
    this.ready = true;
  }

  /** 每帧在 avatar.vrm.update(dt) 之后调用 */
  sync(avatar) {
    if (!this.ready) return;
    const vrm = avatar.vrm;
    vrm.scene.updateMatrixWorld(true);

    // 剔除整体转身：bodyYaw 单独给 root，避免双重旋转
    vrm.scene.getWorldQuaternion(this._invRoot);
    this._invRoot.invert();

    for (const [part, boneKey] of Object.entries(PART_BONE)) {
      const bone = avatar.bones[boneKey];
      const pivot = this.pivots[part];
      const q0 = this.q0[part];
      if (!bone || !pivot || !q0) continue;

      if (part === 'armL' || part === 'armR') {
        // 轴重映射：VRM 前摆 → 正面可见的抬手
        const r0 = this.rest[part];
        const dx = bone.rotation.x - r0.x;
        const dy = bone.rotation.y - r0.y;
        const dz = bone.rotation.z - r0.z;
        const la = avatar.bones[part === 'armL' ? 'lLA' : 'rLA'];
        const lr = part === 'armL' ? this.restLA : this.restRA;
        const ly = la && lr ? la.rotation.y - lr.y : 0;
        const lz = la && lr ? la.rotation.z - lr.z : 0;
        const sign = part === 'armL' ? 1 : -1;
        const lift = Math.max(0, Math.min(1.2, -dx)) * ARM_LIFT;
        pivot.rotation.set(
          -dx * 0.10 + ly * 0.18,                    // 保留一点前后透视
          sign * (dy * 0.20 + lz * 0.12),            // 肘部前送 → 轻微内外转
          sign * lift + dz * 0.18                    // 抬手（正面可见）
        );
        continue;
      }

      bone.getWorldQuaternion(this.q);
      // 回到 vrm.scene 局部坐标 → 相对初始站姿的增量
      const local = this._tmpQ.copy(this._invRoot).multiply(this.q);
      const delta = this._tmpQ2.copy(q0).invert().multiply(local);
      // 幅度缩放：从单位四元数 slerp 到目标增量
      pivot.quaternion.copy(IDENTITY).slerp(delta, GAIN[part]);
      if (part === 'head') {
        // 吉祥物没有嘴部/眨眼网格，Q 版短手的手势天然有限，因此"在讲解"的
        // 观感主要由头与上身的连续节奏承担：慢速左右 + 随语句轻点头。
        const energy = avatar.speaking ? 1 : 0.35;
        pivot.rotateY(Math.sin(avatar.t * 0.9) * 0.030 * energy);
        pivot.rotateX(Math.sin(avatar.t * 2.1) * 0.032 * energy
          * (0.5 + avatar.lastVisAmp));
      }
    }

    this.root.rotation.y = avatar.bodyYaw;
    // 呼吸 / 换重心带来的髋部上下位移（VRM 局部坐标 → 世界需乘 scene 缩放）
    const hips = avatar.bones.hips;
    if (hips) {
      this.root.position.y = (hips.position.y - avatar.hipsY0) * vrm.scene.scale.y
        + Math.sin(avatar.t * 1.1) * 0.006;
      this.root.rotation.z = Math.sin(avatar.t * 0.5) * 0.012;   // 缓慢换重心
    }
  }

  /** 退出吉祥物模式，恢复 VRM 外观 */
  dispose() {
    for (const o of this.hiddenVrmMeshes) o.visible = true;
    this.hiddenVrmMeshes.length = 0;
    this.scene.remove(this.root);
    this.root.traverse((o) => { if (o.isMesh && o.geometry) o.geometry.dispose(); });
    this.ready = false;
  }

  bounds() {
    return new THREE.Box3().setFromObject(this.root, true);
  }
}
