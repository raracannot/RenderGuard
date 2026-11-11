"""RenderGuard 渲染引导脚本（在 Blender 内部执行，必须早于 -a / -f）。

职责：
  1. 应用任务里的参数覆盖（相机 / 分辨率 / 帧率 / 帧范围 / 格式 / 输出路径）
  2. 关闭占位帧（否则空文件会让"已渲染"判断失效）
  3. 设置覆盖/跳过策略 —— 这是续接的核心
  4. 通过渲染回调写心跳文件，供外部守护进程判断"还活着"还是"卡死了"

环境变量：
  RG_JOB        任务 JSON 路径
  RG_HEARTBEAT  心跳文件路径
  RG_OVERWRITE  "1" 覆盖已有帧 / "0" 跳过已有帧（续接）
"""

import bpy
import json
import os
import sys
import time
import traceback

JOB = os.environ.get("RG_JOB", "")
HB = os.environ.get("RG_HEARTBEAT", "")
OVERWRITE = os.environ.get("RG_OVERWRITE", "0") == "1"


def fail(msg):
    print("RG_SETUP_FAILED %s" % msg, flush=True)
    raise RuntimeError(msg)


def load_job():
    if not JOB or not os.path.exists(JOB):
        return {}
    with open(JOB, "r", encoding="utf-8") as fh:
        return json.load(fh)


_state = {"frame": 0, "sample": 0, "samples": 0, "written": 0, "ts": 0.0}
_last_write = [0.0]


def heartbeat(force=False):
    if not HB:
        return
    now = time.time()
    if not force and now - _last_write[0] < 0.9:
        return
    _last_write[0] = now
    _state["ts"] = now
    try:
        tmp = HB + ".tmp"
        with open(tmp, "w", encoding="utf-8") as fh:
            json.dump(_state, fh)
        os.replace(tmp, HB)
    except Exception:
        pass


def _scene(args):
    if args and hasattr(args[0], "render"):
        return args[0]
    return bpy.context.scene


def on_stats(*args):
    try:
        sc = _scene(args)
        if sc is not None:
            _state["frame"] = int(sc.frame_current)
            cy = getattr(sc, "cycles", None)
            if cy is not None:
                _state["samples"] = int(getattr(cy, "samples", 0) or 0)
    except Exception:
        pass
    heartbeat()


def on_write(*args):
    _state["written"] = int(_state.get("written", 0)) + 1
    heartbeat(force=True)


def on_post(*args):
    try:
        sc = _scene(args)
        if sc is not None:
            _state["frame"] = int(sc.frame_current)
    except Exception:
        pass
    heartbeat(force=True)


def on_cancel(*args):
    _state["cancelled"] = True
    heartbeat(force=True)


def restore_cycles_prefs(job):
    """纯净模式（--factory-startup）下恢复 Cycles 的 GPU 设备偏好。

    不恢复的话，scene.cycles.device='GPU' 会因为偏好里 device_type 是默认值
    而静默回落到 CPU —— 渲染一夜才发现全是 CPU 跑的，这是最坑的失败模式。
    """
    dev = job.get("cyclesDevice") or ""
    names = job.get("cyclesDevices") or []
    # "NONE"/"CPU" 表示用 CPU 渲染，场景里已有 device='CPU'，不需要动偏好
    if not dev or dev in ("NONE", "CPU"):
        return
    cy = bpy.context.preferences.addons.get("cycles")
    if not cy:
        return
    try:
        p = cy.preferences
        p.compute_device_type = dev
        p.get_devices()
        for d in p.devices:
            d.use = d.name in names
        print("RG_INFO 已恢复 Cycles 设备偏好: %s / %s" % (dev, names), flush=True)
    except Exception as exc:
        print("RG_WARN 恢复 Cycles 设备偏好失败: %s" % exc, flush=True)


def apply_overrides(sc, ov):
    r = sc.render

    cam = ov.get("camera")
    if cam:
        obj = bpy.data.objects.get(cam)
        if obj is None:
            fail("找不到相机对象: %s" % cam)
        if obj.type != "CAMERA":
            fail("对象不是相机: %s (%s)" % (cam, obj.type))
        sc.camera = obj

    res = ov.get("resolution")
    if res:
        r.resolution_x = int(res[0])
        r.resolution_y = int(res[1])

    rp = ov.get("resolutionPercentage")
    if rp:
        r.resolution_percentage = max(1, min(1000, int(rp)))

    fps = ov.get("fps")
    if fps:
        fps = float(fps)
        if fps <= 0:
            fail("帧率必须大于 0")
        r.fps = max(1, int(round(fps)))
        r.fps_base = r.fps / fps

    if ov.get("frameStart") is not None:
        sc.frame_start = int(ov["frameStart"])
    if ov.get("frameEnd") is not None:
        sc.frame_end = int(ov["frameEnd"])
    if ov.get("frameStep") is not None:
        sc.frame_step = max(1, int(ov["frameStep"]))

    fmt = ov.get("format")
    if fmt:
        try:
            r.image_settings.file_format = fmt
        except Exception:
            fail("不支持的输出格式: %s" % fmt)

    outdir = ov.get("outputDir")
    if outdir:
        outdir = os.path.abspath(outdir)
        if not os.path.isdir(outdir):
            try:
                os.makedirs(outdir, exist_ok=True)
            except Exception as exc:
                fail("无法创建输出目录 %s: %s" % (outdir, exc))
        prefix = ov.get("prefix") or ""
        if prefix:
            r.filepath = os.path.join(outdir, prefix)
        else:
            r.filepath = outdir.rstrip("\\/") + os.sep


def check_tempdir():
    """临时目录落在磁盘根目录时告警。

    Blender 每个会话会在临时目录下建 blender_<pid>；一旦建不出来，它会退回临时目录本身。
    如果那个目录是盘符根目录（例如用户在首选项里填了 R:\\），退出时它就会去清扫整块盘的根，
    删不掉的文件会反复重试 —— 实测能刷出十几万行 "Unable to delete file"、白等一分钟，
    而且能删掉的文件真的会被删掉。
    """
    try:
        tmp = bpy.app.tempdir or ""
    except Exception:
        return
    if os.environ.get("RG_TRACE") == "1":
        try:
            n = len(os.listdir(tmp)) if tmp and os.path.isdir(tmp) else -1
        except Exception as e:
            n = "读不了:%s" % e
        print(
            "RG_TEMP_DIR=%r exists=%s 条目=%s TEMP=%r TMP=%r CWD=%r 变量数=%d"
            % (tmp, os.path.isdir(tmp), n,
               os.environ.get("TEMP"), os.environ.get("TMP"), os.getcwd(),
               len(os.environ))
        )
        # 验证：本进程能否真的在 TEMP 下建目录
        try:
            probe = os.path.join(os.environ.get("TEMP") or ".", "rg_probe_%d" % os.getpid())
            os.mkdir(probe)
            os.rmdir(probe)
            print("RG_TEMP_MKDIR=ok")
        except Exception as e:
            print("RG_TEMP_MKDIR=失败 %s: %s" % (type(e).__name__, e))
    p = tmp.replace("\\", "/").rstrip("/")
    is_drive_root = len(p) == 2 and p[1] == ":"
    if is_drive_root:
        print(
            "RG_TEMP_WARN 该 Blender 的临时目录被设成了磁盘根目录 %s，"
            "渲染退出时可能清扫根目录并卡住约 1 分钟；"
            "请在 编辑 → 偏好设置 → 文件路径 → 临时文件 里清空这一项" % tmp
        )


def setup():
    job = load_job()
    sc = bpy.context.scene
    if sc is None:
        fail("没有活动场景")

    check_tempdir()

    if os.environ.get("RG_CLEAN") == "1":
        restore_cycles_prefs(job)

    apply_overrides(sc, job.get("overrides") or {})

    # 占位帧会写出 0 字节文件，让"跳过已完成帧"失效
    sc.render.use_placeholder = False
    # 续接开关：这是整件事的核心，交给 Blender 自己的 skip 逻辑
    sc.render.use_overwrite = OVERWRITE

    _state["frame"] = int(sc.frame_current)

    for name, fn in (
        ("render_stats", on_stats),
        ("render_write", on_write),
        ("render_post", on_post),
        ("render_cancel", on_cancel),
    ):
        coll = getattr(bpy.app.handlers, name, None)
        if coll is not None:
            try:
                coll.append(fn)
            except Exception:
                pass

    heartbeat(force=True)
    print(
        "RG_SETUP_OK scene=%s overwrite=%s res=%dx%d@%d%% range=%d-%d/%d fmt=%s out=%s"
        % (
            sc.name,
            OVERWRITE,
            sc.render.resolution_x,
            sc.render.resolution_y,
            sc.render.resolution_percentage,
            sc.frame_start,
            sc.frame_end,
            sc.frame_step,
            sc.render.image_settings.file_format,
            sc.render.filepath,
        ),
        flush=True,
    )


try:
    setup()
except Exception:
    traceback.print_exc()
    sys.exit(1)
