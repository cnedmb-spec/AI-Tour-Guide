# -*- coding: utf-8 -*-
"""生成桌面端图标（可重复运行）。只用 Pillow，不引入其它依赖。"""
from pathlib import Path

from PIL import Image, ImageDraw, ImageFont

OUT = Path(__file__).resolve().parent / "assets" / "guide.ico"
PREVIEW = Path(__file__).resolve().parent / "assets" / "guide-preview.png"
S = 256
INK = (12, 10, 8, 255)
GOLD = (200, 146, 74, 255)
CREAM = (233, 214, 180, 255)

img = Image.new("RGBA", (S, S), (0, 0, 0, 0))
d = ImageDraw.Draw(img)
d.rounded_rectangle((10, 10, S - 10, S - 10), radius=54, fill=INK, outline=GOLD, width=7)

font_path = "C:/Windows/Fonts/msyhbd.ttc"
if not Path(font_path).is_file():
    font_path = "C:/Windows/Fonts/simhei.ttf"
font = ImageFont.truetype(font_path, 150)
text = "侨"
box = d.textbbox((0, 0), text, font=font)
w, h = box[2] - box[0], box[3] - box[1]
d.text(((S - w) / 2 - box[0], (S - h) / 2 - box[1] - 10), text, font=font, fill=CREAM)
d.line((78, 214, 178, 214), fill=GOLD, width=5)

OUT.parent.mkdir(parents=True, exist_ok=True)
img.save(OUT, format="ICO", sizes=[(16, 16), (32, 32), (48, 48), (64, 64), (128, 128), (256, 256)])
img.save(PREVIEW)
print("icon:", OUT, OUT.stat().st_size, "bytes")
