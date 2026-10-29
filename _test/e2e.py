"""端到端验证：启动渲染 → 中途强杀 Blender → 观察是否自动重启并续接。

用法（在 render_guard 目录下）：
    set RG_BLENDER=D:\\你的\\Blender\\blender.exe
    python _test\\e2e.py

前提：先构建 dist\\RenderGuard.exe（build.ps1），或把 exe 拷成 _test\\rg.exe。
"""
import glob
import json
import os
import subprocess
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)

EXE = os.path.join(ROOT, "dist", "RenderGuard.exe")
if not os.path.exists(EXE):
    EXE = os.path.join(HERE, "rg.exe")
if not os.path.exists(EXE):
    sys.exit("找不到 RenderGuard.exe，请先运行 build.ps1")

FRAMES = os.path.join(HERE, "frames")
BLEND = os.path.join(HERE, "scene.blend")
if not os.path.exists(BLEND):
    sys.exit("找不到 scene.blend，请先用 Blender 跑一次 make_scene.py")

BLENDER = os.environ.get("RG_BLENDER", "")
if not BLENDER or not os.path.isfile(BLENDER):
    sys.exit("请先设置环境变量 RG_BLENDER 指向 blender.exe")

os.makedirs(FRAMES, exist_ok=True)
for f in glob.glob(os.path.join(FRAMES, "*")):
    os.remove(f)

cfg = {
    "blender": BLENDER,
    "blend": BLEND,
    "scene": "Scene",
    "frameStart": 1,
    "frameEnd": 8,
    "frameStep": 1,
    "animation": True,
    "pathPrefix": os.path.join(FRAMES, "frame_"),
    "pathPad": 4,
    "pathSuffix": ".png",
    "templateOk": True,
    "resume": True,
    "autoRestart": True,
    "maxRetries": 3,
    "verify": True,
    "overrides": {
        "resolution": [480, 480],
        "resolutionPercentage": 100,
        "format": "PNG",
        "outputDir": FRAMES,
        "prefix": "frame_",
    },
    "timeoutSec": 400,
    "resultPath": os.path.join(HERE, "result.json"),
}
cfg_path = os.path.join(HERE, "selftest.json")
with open(cfg_path, "w", encoding="utf-8") as fh:
    json.dump(cfg, fh, ensure_ascii=False, indent=1)

log_path = os.path.join(HERE, "selftest.log")
logf = open(log_path, "w", encoding="utf-8", errors="replace")
p = subprocess.Popen([EXE, "--selftest", cfg_path], stdout=logf,
                     stderr=subprocess.STDOUT, creationflags=0x08000000)

# 等渲染进行到一半，强杀本次测试的 blender 进程
time.sleep(13)
ps = (
    "Get-CimInstance Win32_Process -Filter \"name='blender.exe'\" | "
    "Where-Object { $_.CommandLine -like '*scene.blend*' } | "
    "ForEach-Object { $_.ProcessId }"
)
r = subprocess.run(["powershell", "-NoProfile", "-Command", ps],
                   capture_output=True, text=True)
pids = [x.strip() for x in r.stdout.splitlines() if x.strip().isdigit()]
print("渲染中的 blender 进程:", pids)
for pid in pids:
    subprocess.run(["taskkill", "/PID", pid, "/T", "/F"], capture_output=True)
print("已强杀，模拟崩溃")
print("崩溃时已完成帧数:", len(glob.glob(os.path.join(FRAMES, "*.png"))))

t0 = time.time()
while p.poll() is None and time.time() - t0 < 300:
    time.sleep(1)
if p.poll() is None:
    p.kill()
    print("超时")
logf.close()

result = {}
if os.path.exists(cfg["resultPath"]):
    result = json.load(open(cfg["resultPath"], encoding="utf-8"))

frames = sorted(glob.glob(os.path.join(FRAMES, "*.png")))
sizes = [os.path.getsize(f) for f in frames]

print("\n===== 结果 =====")
print("退出码:", p.returncode)
print("状态:", result.get("status"))
print("完成/总数:", result.get("done"), "/", result.get("total"))
print("重启次数:", result.get("restarts"))
print("失败帧:", result.get("failedFrames"))
print("帧文件数:", len(frames), "最小体积:", min(sizes) if sizes else 0)

ok = (
    result.get("status") == "Done"
    and result.get("done") == 8
    and result.get("restarts", 0) >= 1
    and len(frames) == 8
    and all(s > 1000 for s in sizes)
)
print("\n判定:", "PASS" if ok else "FAIL")
sys.exit(0 if ok else 1)
