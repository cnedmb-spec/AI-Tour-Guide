# -*- coding: utf-8 -*-
"""侨乡 · 江门 品牌标识（最终定稿）—— 一笔碉楼。

用户最终只保留这一个方案：一条连续曲线勾出碉楼轮廓与拱形门洞，
不抬笔、不接笔、不拼贴，没有第二个图形，也没有任何文字。

这一笔怎么走：
    左下起笔 → 左侧收分塔身 → 半圆穹顶 → 右侧收分塔身 → 沿基座向内
    → 拱门右柱 → 半圆券顶 → 拱门左柱 → 回到左下收笔。
    首尾在基座留一个笔口，看得出是一气呵成的一笔。

SVG 与 PNG 由同一份点集生成，形状完全一致；SVG 为纯路径，不依赖字体。
运行：python brand/build_assets.py
"""
import math
from pathlib import Path

from PIL import Image, ImageDraw

OUT = Path(__file__).resolve().parent

INK = (12, 10, 8)
GOLD = (217, 168, 95)
CREAM = (239, 234, 224)

S = 512
CX = CY = 256
MONO_W = 14           # 等线宽


def hx(c):
    return "#%02X%02X%02X" % c


def path_tower():
    """一条连续曲线：碉楼轮廓 + 拱门，首尾在左下基座留笔口。"""
    pts = []
    for i in range(41):                              # 左侧收分塔身
        t = i / 40
        pts.append((172 + (198 - 172) * t, 410 + (250 - 410) * t))
    for i in range(1, 65):                           # 半圆穹顶（左 → 右，过顶）
        p = math.pi - math.pi * i / 64
        pts.append((256 + 58 * math.cos(p), 250 - 58 * math.sin(p)))
    for i in range(1, 41):                           # 右侧收分塔身
        t = i / 40
        pts.append((314 + (340 - 314) * t, 250 + (410 - 250) * t))
    pts.append((300, 410))                           # 基座右段向内
    pts.append((300, 352))
    for i in range(1, 65):                           # 拱门券顶（右 → 左，过顶）
        p = math.pi * i / 64
        pts.append((256 + 44 * math.cos(p), 352 - 44 * math.sin(p)))
    pts.append((212, 410))                           # 拱门左柱 + 基座左段
    pts.append((160, 410))
    s, shift = 1.30, -20                             # 放大上移，撑满方形画布
    return [(256 + (x - 256) * s, 300 + (y - 300) * s + shift)
            for x, y in pts]


POINTS = path_tower()


def scale_points(pts, k):
    return [(round(x * k), round(y * k)) for x, y in pts]


def stroke_scale(size):
    if size >= 256:
        return 1.0
    if size >= 128:
        return 1.3
    return 1.65


def render_png(size, bg=INK, fg=GOLD, ss=4):
    k = size / S * ss
    ws = k * stroke_scale(size)
    im = Image.new("RGB", (size * ss, size * ss), bg)
    d = ImageDraw.Draw(im)
    sp = scale_points(POINTS, k)
    w = max(1, round(MONO_W * ws))
    d.line(sp, fill=fg, width=w, joint="curve")
    r0 = w / 2
    for p in (sp[0], sp[-1]):
        d.ellipse([round(p[0] - r0), round(p[1] - r0),
                   round(p[0] + r0), round(p[1] + r0)], fill=fg)
    return im.resize((size, size), Image.LANCZOS)


def render_svg(bg=INK, fg=GOLD):
    parts = [f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {S} {S}" '
             f'width="{S}" height="{S}">',
             '  <title>Qiaoxiang Jiangmen - One Stroke Tower</title>']
    if bg is not None:
        parts.append(f'  <rect width="{S}" height="{S}" fill="{hx(bg)}"/>')
    line = " ".join(f"{x:.2f},{y:.2f}" for x, y in POINTS)
    parts.append(f'  <polyline points="{line}" fill="none" stroke="{hx(fg)}" '
                 f'stroke-width="{MONO_W}" stroke-linecap="round" '
                 f'stroke-linejoin="round"/>')
    parts.append("</svg>")
    return "\n".join(parts) + "\n"


def make_ico(path):
    sizes = [16, 32, 48, 64, 128, 256]
    render_png(256).save(path, format="ICO",
                         sizes=[(n, n) for n in sizes])


def main():
    OUT.mkdir(parents=True, exist_ok=True)

    for size in (512, 256, 128, 64):
        render_png(size).save(OUT / f"logo-tower-{size}.png")
    (OUT / "logo-tower.svg").write_text(render_svg(), encoding="utf-8")
    make_ico(OUT / "logo-tower.ico")

    # 浅底 / 透明底版
    render_png(512, bg=CREAM, fg=INK).save(OUT / "logo-light.png")
    (OUT / "logo-light.svg").write_text(
        render_svg(bg=CREAM, fg=INK), encoding="utf-8")
    (OUT / "logo-transparent.svg").write_text(
        render_svg(bg=None, fg=GOLD), encoding="utf-8")

    # 默认文件（桌面快捷方式 / favicon 用）
    make_ico(OUT / "logo.ico")
    (OUT / "logo.svg").write_text(render_svg(), encoding="utf-8")
    render_png(512).save(OUT / "logo.png")

    keep = {f"logo-tower-{n}.png" for n in (512, 256, 128, 64)}
    keep |= {"logo-tower.svg", "logo-tower.ico", "logo-light.png",
             "logo-light.svg", "logo-transparent.svg", "logo.svg",
             "logo.png", "logo.ico", "build_assets.py"}
    removed = [p.name for p in OUT.glob("logo-*")
               if p.is_file() and p.name not in keep]
    for name in removed:
        (OUT / name).unlink()
    sheet = OUT / "candidates.png"                   # 对比图已无意义
    if sheet.is_file():
        sheet.unlink()
        removed.append(sheet.name)
    print("final: one stroke tower only")
    print("removed:", removed or "(none)")


if __name__ == "__main__":
    main()
