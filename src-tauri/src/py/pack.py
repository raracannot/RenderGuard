# -*- coding: utf-8 -*-
"""把图片序列打包成一个视频（使用 Blender 内置的 FFmpeg 编码器）。

由 RenderGuard 以 `blender -b --factory-startup --python pack.py -- <config.json>` 调用。
配置字段（JSON）：
    srcDir     序列所在文件夹
    outFile    输出视频的完整路径
    fps        帧率
    order      name_asc / name_desc / time_asc / time_desc
    format     MPEG4 / MKV / QUICKTIME / WEBM
    codec      H264 / H265 / AV1 / WEBM / PRORES ...
    quality    LOSSLESS / PERC_LOSSLESS / HIGH / MEDIUM / LOW / VERYLOW / LOWEST
    audio      可选：音频或视频文件路径（取其音轨）
    bg         可选：透明白底填充色，形如 #101010
"""
import glob
import json
import os
import re
import sys

import bpy

IMAGE_EXT = {".png", ".jpg", ".jpeg", ".tif", ".tiff", ".bmp", ".webp", ".tga", ".exr"}
MOVIE_EXT = {".mp4", ".mov", ".mkv", ".webm", ".avi"}
EXT_BY_FORMAT = {
    "MPEG4": ".mp4",
    "MKV": ".mkv",
    "QUICKTIME": ".mov",
    "WEBM": ".webm",
    "AVI": ".avi",
    "MPEG1": ".mpg",
    "MPEG2": ".mpg",
}


def fail(msg):
    print("PACK_FAIL " + str(msg))
    sys.exit(1)


def natural_key(name):
    parts = re.split(r"(\d+)", name.lower())
    return [int(p) if p.isdigit() else p for p in parts]


def collect(src, order):
    files = []
    for f in os.listdir(src):
        p = os.path.join(src, f)
        if os.path.isfile(p) and os.path.splitext(f)[1].lower() in IMAGE_EXT:
            files.append(f)
    if order == "name_asc":
        files.sort(key=natural_key)
    elif order == "name_desc":
        files.sort(key=natural_key, reverse=True)
    elif order == "time_asc":
        files.sort(key=lambda f: (os.path.getmtime(os.path.join(src, f)), natural_key(f)))
    elif order == "time_desc":
        files.sort(key=lambda f: (os.path.getmtime(os.path.join(src, f)), natural_key(f)), reverse=True)
    else:
        files.sort(key=natural_key)
    return files


def strips_of(se):
    """兼容不同版本：旧版叫 sequences，新版可能叫 strips"""
    return getattr(se, "strips", None) if hasattr(se, "strips") else se.sequences


def hex_to_rgb(s, default=(0.04, 0.04, 0.04)):
    s = (s or "").strip().lstrip("#")
    if len(s) != 6:
        return default
    try:
        return tuple(int(s[i:i + 2], 16) / 255.0 for i in (0, 2, 4))
    except ValueError:
        return default


def main():
    argv = sys.argv[sys.argv.index("--") + 1:]
    if not argv:
        fail("缺少配置文件")
    with open(argv[0], encoding="utf-8") as fh:
        cfg = json.load(fh)

    src = cfg["srcDir"]
    out = cfg["outFile"]
    fps = float(cfg.get("fps") or 30)

    files = collect(src, cfg.get("order", "name_asc"))
    if not files:
        fail("文件夹里没有可用的图片（支持 png/jpg/tif/webp/tga/exr）")

    bpy.ops.wm.read_factory_settings(use_empty=True)
    sc = bpy.context.scene
    se = sc.sequence_editor_create()
    pool = strips_of(se)

    first = os.path.join(src, files[0])
    strip = pool.new_image(name="seq", filepath=first, channel=2, frame_start=1)
    for f in files[1:]:
        strip.elements.append(f)
    strip.frame_final_duration = len(files)

    img = bpy.data.images.load(first)
    width, height = img.size
    if not width or not height:
        fail("第一帧读不出尺寸：%s" % files[0])

    # 尺寸不一致时以首帧为准，并明确告警（否则会静默裁切）
    odd = 0
    for f in files[:: max(1, len(files) // 20)]:
        try:
            bpy.data.images.load(os.path.join(src, f), check_existing=True)
        except Exception:
            odd += 1
    for im in bpy.data.images:
        if im.size[0] and im.size[1] and (im.size[0], im.size[1]) != (width, height):
            odd += 1
    if odd:
        print("PACK_WARN 有 %d 张图的尺寸与首帧 %dx%d 不一致，超出部分会被裁掉" % (odd, width, height))

    sc.render.resolution_x = width
    sc.render.resolution_y = height
    sc.render.resolution_percentage = 100
    sc.render.fps = int(fps)
    sc.render.fps_base = 1.0
    sc.frame_start = 1
    sc.frame_end = len(files)

    # 关键：不做色彩管理换算，否则出来的颜色和 PNG 对不上
    sc.view_settings.view_transform = "Standard"
    sc.view_settings.look = "None"
    try:
        sc.sequencer_colorspace_settings.name = "sRGB"
    except Exception:
        pass

    # 透明背景垫底色（VSE 的空白区域默认是黑的，这里允许自定义）
    sc.render.film_transparent = False
    try:
        sc.world.color = hex_to_rgb(cfg.get("bg"))
    except Exception:
        pass

    audio = (cfg.get("audio") or "").strip()
    if audio and os.path.isfile(audio):
        try:
            pool.new_sound(name="audio", filepath=audio, channel=1, frame_start=1)
            sc.render.ffmpeg.audio_codec = "AAC"
            sc.render.ffmpeg.audio_bitrate = 192
            sc.render.ffmpeg.audio_mixrate = 48000
        except Exception as exc:
            print("PACK_WARN 音轨加载失败：%s" % exc)
    else:
        sc.render.ffmpeg.audio_codec = "NONE"

    r = sc.render
    r.image_settings.file_format = "FFMPEG"
    r.ffmpeg.format = cfg.get("format") or "MPEG4"
    r.ffmpeg.codec = cfg.get("codec") or "H264"
    r.ffmpeg.constant_rate_factor = cfg.get("quality") or "MEDIUM"

    # ProRes 档位：只有 4444 / 4444_XQ 带 alpha
    if r.ffmpeg.codec == "PRORES":
        prof = (cfg.get("prores") or "").strip()
        if prof:
            try:
                r.ffmpeg.ffmpeg_prores_profile = prof
            except Exception as exc:
                print("PACK_WARN ProRes 档位设置失败：%s" % exc)

    # 要保留 alpha 就必须以 RGBA 渲染，否则编码器只会拿到 RGB
    if cfg.get("rgba"):
        try:
            r.image_settings.color_mode = "RGBA"
        except Exception:
            pass

    d = os.path.dirname(out)
    if d:
        os.makedirs(d, exist_ok=True)
    # 去掉扩展名，Blender 会按容器自动补
    root, _ext = os.path.splitext(out)
    r.filepath = root

    print("PACK_INFO files=%d size=%dx%d fps=%s format=%s codec=%s"
          % (len(files), width, height, int(fps), r.ffmpeg.format, r.ffmpeg.codec))
    bpy.ops.render.render(animation=True)

    # Blender 默认会给电影输出加上帧范围后缀（xxx0001-0120.mp4），
    # 这里改回用户指定的文件名。
    ext = EXT_BY_FORMAT.get(r.ffmpeg.format, ".mp4")
    produced = [
        p for p in glob.glob(glob.escape(root) + "*")
        if os.path.isfile(p) and p.lower().endswith(ext)
    ]
    if produced:
        newest = max(produced, key=os.path.getmtime)
        if os.path.abspath(newest) != os.path.abspath(out):
            if os.path.exists(out):
                os.remove(out)
            os.replace(newest, out)

    if not os.path.exists(out):
        fail("编码结束但没有找到产物：%s" % out)
    print("PACK_OK " + out)


if __name__ == "__main__":
    try:
        main()
    except SystemExit:
        raise
    except Exception as exc:  # noqa: BLE001
        import traceback
        traceback.print_exc()
        fail(exc)
