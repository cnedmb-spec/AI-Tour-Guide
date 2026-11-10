"""把用户提供的单网格静态 GLB 转成「按身体部位分件」的可驱动模型。

原始模型：871,064 顶点 / 1,499,984 三角面 / 1 个 mesh / 0 骨骼 / 0 动画 /
7476 个彼此独立的刚性连通块（典型的分件式卡通建模）。

本脚本做四件事：
1. 把 Blender 导出的 Z-up 节点旋转烘焙进顶点（法线同步变换），得到 Y-up 站姿；
2. 用三角形邻接做连通分量，按每个连通块的重心归到 6 个身体部位；
3. 每个部位重新索引成独立 primitive，顶点减去关节枢轴（node.translation 记录枢轴），
   运行时只要绕该枢轴旋转就能让整条手臂 / 头 / 腿动作，不会像自动蒙皮那样撕裂；
4. 3 张 4096x4096 PNG 缩到 2048 JPEG，文件从 ~80MB 降到 ~20MB。

产物：web/assets/models/mascot.glb，运行时由 web/src/mascot.js 重定向。
"""
from __future__ import annotations

import io
import json
import struct
import sys
import time
from pathlib import Path

import numpy as np
from PIL import Image

ROOT = Path(__file__).resolve().parents[1]
SRC = Path(sys.argv[1]) if len(sys.argv) > 1 else Path(
    'C:/Users/ghhjhjh/Desktop/38e0bc8ffd5c608c2372644a1dbe9595.glb')
DST = ROOT / 'web' / 'assets' / 'models' / 'mascot.glb'

# 部位定义：名字 / 关节枢轴（原始单位，Y-up，+Z 为正面）
PARTS = [
    ('torso', (0.000, 0.420, 0.000)),
    ('head',  (0.000, 0.780, 0.000)),
    # 手臂枢轴落在袖口根部：重归类后手臂只剩正面小白手，顶点重心约
    # (±0.330, 0.455, 0.110)，继续用旧的 0.680 肩高位会让手绕头顶画大弧。
    ('armL',  (-0.300, 0.530, 0.060)),
    ('armR',  (0.300, 0.530, 0.060)),
    ('legL',  (-0.120, 0.330, 0.000)),
    ('legR',  (0.120, 0.330, 0.000)),
]


def classify(c: np.ndarray, ymin: float, ymax: float) -> int:
    """连通块重心 → 部位序号。

    Q 版模型头身一体：脸、眼罩、帽檐的连通块重心高低交错，按重心或包围盒
    细切都会出现「帽子转、脸不转」（三轮实拍已证）。改为把中央上半身整体
    归为一个刚体 `head`，只把手臂与双腿单独分出来。转头时整个上半一起微微
    转动，视觉上像身体朝向变化；抬手仍由独立刚体承担，不撕裂、不分离。

    头戴耳机是独立的 396 个连通块（黑耳罩 / 青绿侧环 / 黄绿饰片），重心
    y=0.50..0.83、|x|=0.28..0.47，旧规则里 y>=0.355 & |x|>=0.300 把它们
    整副划进了手臂刚体：讲解时耳机随抬手枢轴上下甩，脸与头带却不动，实拍
    10s/120 帧确认了这一相对位移。现在把 y>=0.50 且 |x|>=0.28 的侧块统一
    归 head，让耳机与头带、脸同属一个刚体；手臂只保留 y<0.50、位于正面
    （z>0.07）的小白手与袖口，顶点重心约 (±0.330, 0.455, 0.110)。
    """
    x, y = float(c[0]), float(c[1])
    if y >= 0.750:
        return 1                      # 帽 / 脸 / 眼罩 / 黑耳 / 耳机球
    if y >= 0.500 and abs(x) >= 0.280:
        return 1                      # 整副头戴耳机，随头部同步，不再独立摆动
    if y >= 0.355 and x <= -0.300:
        return 2                      # 左臂（含拳）
    if y >= 0.355 and x >= 0.300:
        return 3                      # 右臂（含拳）
    if y <= 0.335:
        return 4 if x < 0 else 5       # 左 / 右腿与鞋
    if y >= 0.450:
        return 1                      # 中央上半（脸下沿 / 胸饰）随头一起动
    return 0                          # 腰与下身


def read_glb(path: Path):
    b = path.read_bytes()
    if b[:4] != b'glTF':
        raise ValueError('不是 GLB 文件')
    jlen = struct.unpack('<I', b[12:16])[0]
    j = json.loads(b[20:20 + jlen].decode('utf-8'))
    bin0 = 12 + 8 + jlen + 8
    bv = j['bufferViews']
    acc = j['accessors']

    def arr(i):
        a = acc[i]
        n = {'SCALAR': 1, 'VEC2': 2, 'VEC3': 3, 'VEC4': 4}[a['type']]
        dt = {5126: np.float32, 5125: np.uint32, 5123: np.uint16}[a['componentType']]
        return np.frombuffer(b, dtype=dt, count=a['count'] * n,
                             offset=bin0 + bv[a['bufferView']]['byteOffset']
                             + a.get('byteOffset', 0)).reshape(-1, n)

    prim = j['meshes'][0]['primitives'][0]
    pos = arr(prim['attributes']['POSITION'])
    nor = arr(prim['attributes']['NORMAL'])
    uv = arr(prim['attributes']['TEXCOORD_0'])
    idx = arr(prim['indices']).reshape(-1, 3)
    imgs = []
    for im in j['images']:
        v = bv[im['bufferView']]
        s = bin0 + v['byteOffset']
        imgs.append(Image.open(io.BytesIO(b[s:s + v['byteLength']])).convert('RGB'))
    return j, pos, nor, uv, idx, imgs


def connected_components(idx: np.ndarray, n: int) -> np.ndarray:
    """numpy 版并查集：hooking + 指针跳跃，返回每个顶点的连通块编号。"""
    e0 = np.concatenate([idx[:, 0], idx[:, 1], idx[:, 2]])
    e1 = np.concatenate([idx[:, 1], idx[:, 2], idx[:, 0]])
    lab = np.arange(n, dtype=np.int32)
    for it in range(400):
        for _ in range(30):
            new = lab[lab]
            if np.array_equal(new, lab):
                break
            lab = new
        a, c = lab[e0], lab[e1]
        np.minimum.at(lab, e0, np.minimum(a, c))
        np.minimum.at(lab, e1, np.minimum(a, c))
        if not np.any(a != c):
            break
    _, inv = np.unique(lab, return_inverse=True)
    print(f'连通分量收敛 {it + 1} 轮，独立块 {inv.max() + 1}')
    return inv.astype(np.int32)


def main():
    t0 = time.time()
    j, pos, nor, uv, idx, imgs = read_glb(SRC)
    n = len(pos)
    # 节点 quaternion [w=0.7071, x=0.7071] = 绕 X 轴 +90°：(x,y,z) -> (x,-z,y)
    P = np.stack([pos[:, 0], -pos[:, 2], pos[:, 1]], axis=1).astype(np.float32)
    N = np.stack([nor[:, 0], -nor[:, 2], nor[:, 1]], axis=1).astype(np.float32)
    UV = uv.astype(np.float32)

    inv = connected_components(idx, n)
    nblocks = int(inv.max()) + 1
    sums = np.zeros((nblocks, 3), dtype=np.float64)
    np.add.at(sums, inv, P.astype(np.float64))
    counts = np.bincount(inv, minlength=nblocks)
    centers = sums / np.maximum(counts[:, None], 1)
    ymin = np.full(nblocks, np.inf)
    ymax = np.full(nblocks, -np.inf)
    np.minimum.at(ymin, inv, P[:, 1])
    np.maximum.at(ymax, inv, P[:, 1])
    block_part = np.array(
        [classify(centers[i], float(ymin[i]), float(ymax[i])) for i in range(nblocks)],
        dtype=np.uint8)
    tri_part = block_part[inv[idx[:, 0]]]

    # 输出 GLB 容器
    buf = bytearray()
    bvs, accs, meshes, nodes = [], [], [], []

    def add_bv(data: bytes, target=None):
        buf.extend(b'\0' * ((-len(buf)) % 4))
        off = len(buf)
        buf.extend(data)
        bv = {'buffer': 0, 'byteOffset': off, 'byteLength': len(data)}
        if target:
            bv['target'] = target
        bvs.append(bv)
        return len(bvs) - 1

    def add_acc(bvi, ctype, typ, count, mn=None, mx=None):
        a = {'bufferView': bvi, 'componentType': ctype, 'count': int(count), 'type': typ}
        if mn is not None:
            a['min'] = [float(v) for v in mn]
            a['max'] = [float(v) for v in mx]
        accs.append(a)
        return len(accs) - 1

    for pi, (name, pivot) in enumerate(PARTS):
        tri = idx[tri_part == pi]
        if not len(tri):
            raise RuntimeError(f'部位 {name} 没有任何三角面，分类阈值需要调整')
        used, local = np.unique(tri, return_inverse=True)
        li = local.astype(np.uint32)
        p = P[used] - np.array(pivot, dtype=np.float32)
        nn = N[used]
        uu = UV[used]
        bp = add_bv(p.astype('<f4').tobytes(), 34962)
        bn = add_bv(nn.astype('<f4').tobytes(), 34962)
        bu = add_bv(uu.astype('<f4').tobytes(), 34962)
        bi = add_bv(li.astype('<u4').tobytes(), 34963)
        ap = add_acc(bp, 5126, 'VEC3', len(p), p.min(0), p.max(0))
        an = add_acc(bn, 5126, 'VEC3', len(nn))
        au = add_acc(bu, 5126, 'VEC2', len(uu))
        # SCALAR 索引 accessor 的 count 必须是索引总数（三角面数 × 3），
        # 写成行数会让加载器只读到 1/3 的三角形，模型出现大面积缺面。
        ai = add_acc(bi, 5125, 'SCALAR', li.size)
        meshes.append({'name': name, 'primitives': [{
            'attributes': {'POSITION': ap, 'NORMAL': an, 'TEXCOORD_0': au},
            'indices': ai, 'material': 0,
        }]})
        nodes.append({'name': name, 'mesh': pi,
                      'translation': [float(v) for v in pivot]})
        print(f'{name:6s} 三角面 {len(tri):7d}  顶点 {len(used):7d}  '
              f'枢轴 {pivot}  局部包围盒 y {p[:, 1].min():+.3f}..{p[:, 1].max():+.3f}')

    # 贴图缩到 2048 JPEG：卡通材质用有损压缩看不出差别，体积省 3/4
    img_bvs = []
    for im in imgs:
        small = im.resize((2048, 2048), Image.LANCZOS)
        out = io.BytesIO()
        small.save(out, format='JPEG', quality=88, subsampling=0)
        img_bvs.append(add_bv(out.getvalue()))

    gltf = {
        'asset': {'generator': 'build_mascot.py / 分件重定向', 'version': '2.0'},
        'scene': 0,
        'scenes': [{'nodes': list(range(len(nodes)))}],
        'nodes': nodes,
        'meshes': meshes,
        'materials': [{
            'name': 'Mascot', 'doubleSided': True,
            'normalTexture': {'index': 0},
            'pbrMetallicRoughness': {
                'baseColorTexture': {'index': 1},
                'metallicRoughnessTexture': {'index': 2},
                'metallicFactor': 1.0, 'roughnessFactor': 1.0,
            },
        }],
        'textures': [{'sampler': 0, 'source': i} for i in range(3)],
        'images': [{'bufferView': v, 'mimeType': 'image/jpeg',
                    'name': nm} for v, nm in zip(img_bvs, ['normal', 'basecolor', 'metalrough'])],
        'samplers': [{'magFilter': 9729, 'minFilter': 9987, 'wrapS': 10497, 'wrapT': 10497}],
        'accessors': accs,
        'bufferViews': bvs,
        'buffers': [{'byteLength': len(buf)}],
    }
    js = json.dumps(gltf, separators=(',', ':'), ensure_ascii=False).encode('utf-8')
    js += b' ' * ((-len(js)) % 4)
    buf.extend(b'\0' * ((-len(buf)) % 4))
    total = 12 + 8 + len(js) + 8 + len(buf)
    out = bytearray()
    out += struct.pack('<4sII', b'glTF', 2, total)
    out += struct.pack('<I4s', len(js), b'JSON') + js
    out += struct.pack('<I4s', len(buf), b'BIN\0') + bytes(buf)
    DST.parent.mkdir(parents=True, exist_ok=True)
    DST.write_bytes(out)
    print(f'输出 {DST}  {len(out):,} B（原始 {SRC.stat().st_size:,} B，'
          f'缩小 {(1 - len(out) / SRC.stat().st_size) * 100:.1f}%）  耗时 {time.time() - t0:.1f}s')


if __name__ == '__main__':
    main()
