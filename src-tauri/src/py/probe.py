"""RenderGuard 场景探测脚本（在 Blender 内部执行）。

用法：
    blender -b <file.blend> --python probe.py --python-exit-code 1
环境变量：
    RG_PROBE_OUT  结果 JSON 的输出路径

输出里面包含了任务编辑器需要的全部可覆盖参数，以及每帧输出文件的命名规则
（拆成 prefix / pad / suffix 三段，避免在 Rust 侧解析 Python 格式串）。
"""

import bpy
import json
import os
import re
import sys
import traceback

OUT = os.environ.get("RG_PROBE_OUT", "")

EXT_MAP = {
    "PNG": ".png",
    "JPEG": ".jpg",
    "OPEN_EXR": ".exr",
    "OPEN_EXR_MULTILAYER": ".exr",
    "TIFF": ".tiff",
    "BMP": ".bmp",
    "TARGA": ".tga",
    "TARGA_RAW": ".tga",
    "WEBP": ".webp",
    "DPX": ".dpx",
    "CINEON": ".cin",
    "HDR": ".hdr",
    "IRIS": ".rgb",
    "JP2": ".jp2",
}
MOVIE_FORMATS = {"FFMPEG", "AVI_JPEG", "AVI_RAW"}


def norm(p):
    return os.path.normcase(os.path.normpath(p)) if p else ""


def split_template(path, sample_frame):
    """把绝对输出路径拆成 prefix / pad / suffix，并用 Blender 的 frame_path 校验。"""
    m = None
    for mm in re.finditer(r"#+", path):
        m = mm
    if m:
        prefix, pad, suffix = path[: m.start()], len(m.group(0)), path[m.end():]
    else:
        prefix, pad, suffix = path, 4, ""
    return prefix, pad, suffix


def probe_scene(sc):
    r = sc.render
    fmt = r.image_settings.file_format
    is_movie = fmt in MOVIE_FORMATS

    info = {
        "name": sc.name,
        "engine": r.engine,
        "frameStart": int(sc.frame_start),
        "frameEnd": int(sc.frame_end),
        "frameStep": int(sc.frame_step),
        "cameras": [o.name for o in sc.objects if o.type == "CAMERA"],
        "activeCamera": sc.camera.name if sc.camera else None,
        "resolution": [int(r.resolution_x), int(r.resolution_y)],
        "resolutionPercentage": int(r.resolution_percentage),
        "fps": round(float(r.fps) / float(r.fps_base or 1.0), 4),
        "filepath": bpy.path.abspath(r.filepath or ""),
        "format": fmt,
        "colorMode": r.image_settings.color_mode,
        "colorDepth": r.image_settings.color_depth,
        "isMovie": bool(is_movie),
        "useOverwrite": bool(r.use_overwrite),
        "usePlaceholder": bool(r.use_placeholder),
        "pathPrefix": "",
        "pathPad": 4,
        "pathSuffix": "",
        "templateOk": False,
    }

    if not is_movie:
        try:
            abs_fp = bpy.path.abspath(r.filepath or "")
            if abs_fp:
                prefix, pad, suffix = split_template(abs_fp, sc.frame_start)
                for ext in (EXT_MAP.get(fmt, ""), ""):
                    cand = prefix + str(sc.frame_start).zfill(pad) + suffix + ext
                    try:
                        ref = r.frame_path(frame=sc.frame_start)
                    except Exception:
                        ref = ""
                    if ref and norm(cand) == norm(ref):
                        # 再验一帧，确认补零宽度没算错
                        cand2 = prefix + str(sc.frame_start + 1).zfill(pad) + suffix + ext
                        try:
                            ref2 = r.frame_path(frame=sc.frame_start + 1)
                        except Exception:
                            ref2 = ""
                        if ref2 and norm(cand2) == norm(ref2):
                            info.update(
                                pathPrefix=prefix,
                                pathPad=pad,
                                pathSuffix=suffix + ext,
                                templateOk=True,
                            )
                        break
        except Exception as exc:  # 模板推导失败不影响其余字段
            info["templateError"] = str(exc)

    return info


def main():
    data = {
        "ok": False,
        "error": "",
        "blenderVersion": bpy.app.version_string,
        "blendPath": bpy.data.filepath,
        "activeScene": bpy.context.scene.name if bpy.context.scene else "",
        "scenes": [],
        "cyclesDevice": "",
        "cyclesDevices": [],
    }
    # 纯净模式（--factory-startup）会丢掉 Cycles 的 GPU 设备偏好，
    # 这里把它记下来，渲染时由 boot.py 还原，避免静默回落到 CPU。
    try:
        cy = bpy.context.preferences.addons.get("cycles")
        if cy:
            cp = cy.preferences
            data["cyclesDevice"] = cp.compute_device_type
            data["cyclesDevices"] = [d.name for d in cp.devices if d.use]
    except Exception:
        pass
    try:
        for sc in bpy.data.scenes:
            data["scenes"].append(probe_scene(sc))
        data["ok"] = True
    except Exception:
        data["error"] = traceback.format_exc()

    if OUT:
        tmp = OUT + ".tmp"
        with open(tmp, "w", encoding="utf-8") as fh:
            json.dump(data, fh, ensure_ascii=False, indent=1)
        os.replace(tmp, OUT)

    print("RG_PROBE_DONE ok=%s scenes=%d" % (data["ok"], len(data["scenes"])))
    if not data["ok"]:
        sys.exit(1)


main()
