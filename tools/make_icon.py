"""生成应用图标：深色底 + 琥珀色进度环 + 播放三角，暗示"渲染 / 续接"。"""
import math
from PIL import Image, ImageDraw

S = 1024
img = Image.new("RGBA", (S, S), (0, 0, 0, 0))
d = ImageDraw.Draw(img)

# 圆角深色底
pad, r = 40, 200
d.rounded_rectangle([pad, pad, S - pad, S - pad], radius=r, fill=(21, 24, 29, 255))

# 底部微光
glow = Image.new("RGBA", (S, S), (0, 0, 0, 0))
gd = ImageDraw.Draw(glow)
gd.ellipse([S * 0.1, S * 0.55, S * 0.9, S * 1.05], fill=(255, 176, 54, 26))
img.alpha_composite(glow)

cx = cy = S / 2
R = S * 0.29
w = int(S * 0.075)

# 轨道
d.arc([cx - R, cy - R, cx + R, cy + R], start=0, end=360,
      fill=(58, 64, 74, 255), width=w)

# 已完成进度环（约 72%），从 -90 度开始
d.arc([cx - R, cy - R, cx + R, cy + R], start=-90, end=-90 + 360 * 0.72,
      fill=(255, 176, 54, 255), width=w)

# 端点圆头
for ang in (-90, -90 + 360 * 0.72):
    a = math.radians(ang)
    px, py = cx + R * math.cos(a), cy + R * math.sin(a)
    d.ellipse([px - w / 2, py - w / 2, px + w / 2, py + w / 2],
              fill=(255, 176, 54, 255))

# 中心播放三角
t = S * 0.115
d.polygon([(cx - t * 0.72, cy - t), (cx - t * 0.72, cy + t), (cx + t * 0.95, cy)],
          fill=(236, 239, 244, 255))

img.save(r"E:\RAYS\Desktop\workspace\render_guard\src-tauri\icons\icon.png")
img.resize((256, 256), Image.LANCZOS).save(
    r"E:\RAYS\Desktop\workspace\render_guard\src-tauri\icons\128x128.png")
ico_sizes = [(16, 16), (24, 24), (32, 32), (48, 48), (64, 64), (128, 128), (256, 256)]
img.save(r"E:\RAYS\Desktop\workspace\render_guard\src-tauri\icons\icon.ico",
         sizes=ico_sizes)
print("ICON_OK")
