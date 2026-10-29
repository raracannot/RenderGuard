/* RenderGuard 前端逻辑（原生 JS，无构建步骤） */

const T = window.__TAURI__ || {};
const invoke = T.core && T.core.invoke;
const listen = T.event && T.event.listen;
const dlg = T.dialog;

const EXT = {
  PNG: ".png", JPEG: ".jpg", OPEN_EXR: ".exr", OPEN_EXR_MULTILAYER: ".exr",
  TIFF: ".tiff", BMP: ".bmp", WEBP: ".webp", TARGA: ".tga", TARGA_RAW: ".tga",
  DPX: ".dpx", CINEON: ".cin", HDR: ".hdr", IRIS: ".rgb", JP2: ".jp2",
};
const FORMATS = [
  ["PNG", "PNG"], ["JPEG", "JPEG"], ["OPEN_EXR", "OpenEXR"], ["OPEN_EXR_MULTILAYER", "OpenEXR 多图层"],
  ["TIFF", "TIFF"], ["BMP", "BMP"], ["WEBP", "WebP"], ["TARGA", "Targa"],
  ["FFMPEG", "视频 (FFmpeg)"],
];
const MOVIE = new Set(["FFMPEG", "AVI_JPEG", "AVI_RAW"]);
const STATUS = {
  queued: "排队", running: "渲染中", done: "完成",
  partial: "部分完成", failed: "失败", stopped: "已停止",
};

const S = {
  jobs: new Map(),
  order: [],
  sel: new Set(),
  settings: null,
  blenders: [],
  probe: null,
  scene: null,
  editing: null,
  logLines: 0,
  autoScroll: true,
  showNoise: false,
};

const $ = (id) => document.getElementById(id);

/* ------------------------------------------------------------ 小工具 */

function dirOf(p) {
  if (!p) return "";
  const i = Math.max(p.lastIndexOf("\\"), p.lastIndexOf("/"));
  return i >= 0 ? p.slice(0, i) : "";
}
function baseOf(p) {
  if (!p) return "";
  const i = Math.max(p.lastIndexOf("\\"), p.lastIndexOf("/"));
  return i >= 0 ? p.slice(i + 1) : p;
}
function joinPath(dir, name) {
  if (!dir) return name;
  const sep = dir.endsWith("\\") || dir.endsWith("/") ? "" : "\\";
  return dir + sep + name;
}
function pad4(n) {
  return String(n).padStart(4, "0");
}
function toast(msg, kind) {
  const el = document.createElement("div");
  el.textContent = msg;
  el.style.cssText =
    "position:fixed;left:50%;bottom:260px;transform:translateX(-50%);z-index:99;" +
    "padding:9px 16px;border-radius:8px;font-size:13px;box-shadow:0 8px 30px rgba(0,0,0,.5);" +
    "background:" + (kind === "err" ? "#3a1d1c" : "#1e242d") + ";border:1px solid " +
    (kind === "err" ? "#7a3a36" : "#333c49") + ";color:" + (kind === "err" ? "#ff9d96" : "#dfe4ec");
  document.body.appendChild(el);
  setTimeout(() => el.remove(), 2600);
}

/* ------------------------------------------------------------ 控制台 */

let lastLogKey = "";

function log(text, level, jobName) {
  if (level === "dim" && !S.showNoise) return;
  const key = (jobName || "") + "|" + text;
  if (key === lastLogKey && level !== "error") return;
  lastLogKey = key;

  const box = $("console");
  const atBottom = box.scrollTop + box.clientHeight >= box.scrollHeight - 40;

  const ln = document.createElement("div");
  ln.className = "ln " + (level || "info");

  const ts = document.createElement("span");
  ts.className = "ts";
  const d = new Date();
  ts.textContent =
    String(d.getHours()).padStart(2, "0") + ":" +
    String(d.getMinutes()).padStart(2, "0") + ":" +
    String(d.getSeconds()).padStart(2, "0");

  ln.appendChild(ts);
  if (jobName) {
    const tag = document.createElement("span");
    tag.className = "tag";
    tag.textContent = "[" + jobName + "]";
    ln.appendChild(tag);
  }
  const body = document.createElement("span");
  body.textContent = text;
  ln.appendChild(body);
  box.appendChild(ln);

  S.logLines++;
  if (S.logLines > 1600) {
    while (box.childElementCount > 1500) box.removeChild(box.firstChild);
    S.logLines = box.childElementCount;
  }
  if (atBottom || S.autoScroll) box.scrollTop = box.scrollHeight;
}

/* ------------------------------------------------------------ 队列渲染 */

function selOrAll() {
  if (S.sel.size) return [...S.sel];
  return S.order.slice();
}

function progressCell(job) {
  const p = job.progress || {};
  const pct = Math.max(0, Math.min(100, p.percent || 0));
  const cls = job.status === "done" ? "bar done" : "bar";
  return `<div class="prog" style="--p:${pct.toFixed(1)}">
      <div class="${cls}"><i style="width:${pct.toFixed(1)}%"></i></div>
      <span>${p.done || 0} / ${p.total || 0}${p.failed ? " · 失败 " + p.failed : ""}</span>
    </div>`;
}

function rowHTML(job) {
  const range = job.animation
    ? `${job.frameStart}–${job.frameEnd}${job.frameStep > 1 ? " /" + job.frameStep : ""}`
    : `静帧 ${job.stillFrame}`;
  const cur = job.status === "running" ? (job.progress.current || "—") : "—";
  const retry = job.progress.restarts ? job.progress.restarts : "";
  const sub = job.templateOk ? "" : `<small>模板不可用</small>`;
  return `
    <div class="cell name" title="${esc(job.name)}">${esc(job.name)}${sub}</div>
    <div class="cell" title="${esc(job.scene)}">${esc(job.scene)}</div>
    <div class="cell mono">${range}</div>
    <div class="cell">${esc((job.engine || "").replace("CYCLES", "Cycles").replace("BLENDER_EEVEE_NEXT", "EEVEE").replace("BLENDER_EEVEE", "EEVEE"))}</div>
    <div class="cell"><span class="chip ${job.status}">${STATUS[job.status] || job.status}</span></div>
    <div class="cell">${progressCell(job)}</div>
    <div class="cell mono">${cur}</div>
    <div class="cell mono">${retry}</div>`;
}

function esc(s) {
  return String(s == null ? "" : s).replace(/[&<>"']/g, (c) =>
    ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c]));
}

function renderQueue() {
  const body = $("queueBody");
  body.innerHTML = "";
  for (const id of S.order) {
    const job = S.jobs.get(id);
    if (!job) continue;
    const row = document.createElement("div");
    row.className = "row" + (S.sel.has(id) ? " sel" : "");
    row.dataset.id = id;
    row.innerHTML = rowHTML(job);
    body.appendChild(row);
  }
  $("queueEmpty").classList.toggle("hidden", S.order.length > 0);
  $("queueCount").textContent = String(S.order.length);
  updateLive();
}

function updateRow(job) {
  const row = document.querySelector(`.row[data-id="${CSS.escape(job.id)}"]`);
  if (!row) {
    renderQueue();
    return;
  }
  row.innerHTML = rowHTML(job);
  row.classList.toggle("sel", S.sel.has(job.id));
  updateLive();
}

function updateLive() {
  const jobs = [...S.jobs.values()];
  const anyRunning = jobs.some((j) => j.status === "running");
  $("liveDot").classList.toggle("on", anyRunning);

  // 状态条（GBA 等皮肤用）：把当前/下一个任务摊开显示
  const strip = $("liveStrip");
  if (!strip) return;
  const run = jobs.find((j) => j.status === "running");
  const next = jobs.find((j) => j.status === "queued");
  const done = jobs.filter((j) => j.status === "done").length;
  const p = (run && run.progress) || {};
  strip.innerHTML = `
    <div class="ls-live">
      <b>${run ? p.done || 0 : "—"}</b>
      <span>${run ? "/ " + (p.total || 0) + " 帧" : "空闲"}</span>
    </div>
    <div class="ls-mid">
      <div class="ls-now"><i>NOW</i>${run ? esc(run.name) + " · 第 " + (p.current || 0) + " 帧" : "队列空闲"}</div>
      <div class="ls-next"><i>NEXT</i>${next ? esc(next.name) : "—"}</div>
    </div>
    <div class="ls-stat">
      <div>完成 ${done} / ${jobs.length}</div>
      <div>重启 ${run ? p.restarts || 0 : 0}</div>
    </div>`;
}

/* ------------------------------------------------------------ 详情 */

function renderDetail() {
  const box = $("detail");
  const id = [...S.sel][0];
  const job = id && S.jobs.get(id);
  if (!job) {
    box.innerHTML = `<div class="empty dim">选择左侧任务查看详情</div>`;
    return;
  }
  const p = job.progress || {};
  const outDir = dirOf(job.pathPrefix) || "（未探测）";
  const failed = (job.failedFrames || []).map((f) => `<span>${f}</span>`).join("");
  box.innerHTML = `
    <h3>${esc(job.name)}</h3>
    <dl class="kv">
      <dt>状态</dt><dd><span class="chip ${job.status}">${STATUS[job.status] || job.status}</span></dd>
      <dt>场景</dt><dd>${esc(job.scene)}</dd>
      <dt>帧范围</dt><dd>${job.animation ? `${job.frameStart} – ${job.frameEnd}（步 ${job.frameStep}）` : `静帧 ${job.stillFrame}`}</dd>
      <dt>进度</dt><dd>${p.done || 0} / ${p.total || 0} 帧${p.failed ? `，失败 ${p.failed}` : ""}</dd>
      <dt>用时</dt><dd>${fmtSec(job.elapsed)}</dd>
      <dt>重启</dt><dd>${p.restarts || 0} 次</dd>
      <dt>输出</dt><dd>${esc(outDir)}</dd>
      <dt>工程</dt><dd>${esc(job.blend)}</dd>
      <dt>Blender</dt><dd>${esc(job.blender)}</dd>
      <dt>提示</dt><dd>${esc(p.message || "—")}</dd>
    </dl>
    ${failed ? `<div class="fails"><div class="dim small">失败帧（已隔离）</div><div class="list">${failed}</div></div>` : ""}
    <div class="actions">
      <button class="btn small" data-act="open">输出目录</button>
      <button class="btn small" data-act="log">日志</button>
      <button class="btn small" data-act="blender">Blender 中编辑</button>
      <button class="btn small" data-act="edit">编辑</button>
      <button class="btn small" data-act="retry">重试失败帧</button>
    </div>
    ${
      job.templateOk
        ? ""
        : `<div class="warn-text" style="margin-top:12px">
             输出路径不可用${job.isMovie ? "（视频格式无法帧级续接）" : "（.blend 里没有设置输出路径）"}，
             续接功能对这个任务无效。在任务里勾选「输出路径」覆盖即可修复。
           </div>`
    }`;
  box.querySelectorAll("button[data-act]").forEach((b) => {
    b.onclick = () => {
      const act = b.dataset.act;
      if (act === "open") invoke("open_path", { path: outDir }).catch((e) => toast(String(e), "err"));
      if (act === "log")
        invoke("log_path", { id: job.id })
          .then((p2) => invoke("open_path", { path: p2 }))
          .catch((e) => toast(String(e), "err"));
      if (act === "edit") openJobDialog(job);
      if (act === "blender")
        invoke("open_in_blender", { blender: job.blender, blend: job.blend }).catch((e) =>
          toast(String(e).slice(0, 200), "err")
        );
      if (act === "retry") invoke("retry_failed", { ids: [job.id] }).then(refresh);
    };
  });
}

function fmtSec(s) {
  s = Math.floor(s || 0);
  if (s < 60) return s + " 秒";
  const m = Math.floor(s / 60);
  if (m < 60) return m + " 分 " + (s % 60) + " 秒";
  return Math.floor(m / 60) + " 时 " + (m % 60) + " 分";
}

/* ------------------------------------------------------------ 数据同步 */

async function refresh() {
  const jobs = await invoke("list_jobs");
  S.jobs.clear();
  S.order = [];
  for (const j of jobs) {
    S.jobs.set(j.id, j);
    S.order.push(j.id);
  }
  for (const id of [...S.sel]) if (!S.jobs.has(id)) S.sel.delete(id);
  renderQueue();
  renderDetail();
  noteQueueState();
}

function upsertJob(job) {
  const isNew = !S.jobs.has(job.id);
  S.jobs.set(job.id, job);
  if (isNew) {
    S.order.push(job.id);
    renderQueue();
  } else {
    updateRow(job);
  }
  renderDetail();
  noteQueueState();
}

/* ------------------------------------------------------------ 已打开的 Blender */

async function refreshOpenBlenders() {
  const box = $("openBlenders");
  const row = $("openBlendersRow");
  if (!box || !row) return;
  box.innerHTML = "";
  let list = [];
  try {
    list = await invoke("list_open_blenders");
  } catch (e) {
    list = [];
  }
  if (!list.length) {
    row.classList.add("hidden");
    return;
  }
  row.classList.remove("hidden");
  for (const b of list) {
    const chip = document.createElement("button");
    chip.className = "ob-chip";
    chip.textContent = b.file ? b.name : `${b.name}（未保存）`;
    chip.title = b.file ? b.file : "这个工程还没保存，无法直接添加";
    chip.disabled = !b.file;
    chip.onclick = () => {
      if (!b.file) return toast("先保存这个工程", "err");
      $("inpBlend").value = b.file;
      const sel = $("selBlender");
      if (b.exe && ![...sel.options].some((o) => o.value === b.exe)) {
        const o = document.createElement("option");
        o.value = b.exe;
        o.textContent = b.exe;
        sel.appendChild(o);
      }
      if (b.exe) sel.value = b.exe;
      toast(`已填入：${b.name}`);
      runProbe();
    };
    box.appendChild(chip);
  }
}

/* ------------------------------------------------------------ 任务弹窗 */

function fillFormatSelect() {
  const sel = $("inpFormat");
  sel.innerHTML = "";
  for (const [v, label] of FORMATS) {
    const o = document.createElement("option");
    o.value = v;
    o.textContent = label;
    sel.appendChild(o);
  }
}

function openJobDialog(existing) {
  S.editing = existing || null;
  S.probe = null;
  S.scene = null;
  $("dlgTitle").textContent = existing ? "编辑任务" : "新建渲染任务";
  $("probeStatus").textContent = "";
  $("probeStatus").className = "status-text";
  $("sceneList").innerHTML = `<div class="dim small">先选择 Blender 与工程文件，然后点「读取参数」</div>`;

  const sel = $("selBlender");
  sel.innerHTML = "";

  // 合并「默认路径 / 历史路径 / 自动探测结果」，去重但保留优先级顺序。
  // 之前只用了启动时缓存的探测列表，导致设置里新填的默认路径不在选项里，
  // select.value 赋值失败，界面就显示成空的。
  const known = [];
  const pushPath = (p) => {
    const v = (p || "").trim();
    if (!v) return;
    if (known.some((x) => x.toLowerCase() === v.toLowerCase())) return;
    known.push(v);
  };
  pushPath(S.settings?.defaultBlender);
  for (const p of S.settings?.blenderPaths || []) pushPath(p);
  pushPath(S.settings?.lastBlender);
  for (const p of S.blenders || []) pushPath(p);

  const list = known.length ? known : [""];
  for (const b of list) {
    const o = document.createElement("option");
    o.value = b;
    o.textContent = b || "（未找到，请手动选择）";
    sel.appendChild(o);
  }
  const prefer = existing?.blender || S.settings?.defaultBlender || list[0] || "";
  sel.value = prefer;
  if (sel.selectedIndex < 0) sel.selectedIndex = 0;
  $("inpBlend").value = existing?.blend || "";

  ["chkCamera", "chkRes", "chkFps", "chkRange", "chkType", "chkFormat", "chkOut"].forEach((k) => {
    $(k).checked = false;
  });
  syncOverrideInputs();

  $("chkResume").checked = existing ? existing.resume : (S.settings?.defaultResume ?? true);
  $("chkAutoRestart").checked = existing ? existing.autoRestart : true;
  $("chkVerify").checked = existing ? existing.verify : true;
  $("inpRetries").value = existing ? existing.maxRetries : (S.settings?.defaultMaxRetries ?? 3);

  clearProbeFields();
  $("dlgOverlay").classList.remove("hidden");
  refreshOpenBlenders();

  if (existing && existing.blend) {
    runProbe().then(() => {
      const sc = (S.probe?.scenes || []).find((s) => s.name === existing.scene);
      if (sc) selectScene(sc, existing);
    });
  }
}

function clearProbeFields() {
  $("inpCamera").innerHTML = "";
  $("inpResW").value = "";
  $("inpResH").value = "";
  $("inpResPct").value = "100";
  $("inpFps").value = "";
  $("inpStart").value = "1";
  $("inpEnd").value = "1";
  $("inpStep").value = "1";
  $("inpType").value = "anim";
  $("inpStill").value = "1";
  $("inpOutDir").value = "";
  $("inpPrefix").value = "frame_";
  $("outPreview").textContent = "—";
  $("movieWarn").classList.add("hidden");
}

function syncOverrideInputs() {
  const map = {
    chkCamera: ["inpCamera"],
    chkRes: ["inpResW", "inpResH", "inpResPct"],
    chkFps: ["inpFps"],
    chkRange: ["inpStart", "inpEnd", "inpStep"],
    chkType: ["inpType"],
    chkFormat: ["inpFormat"],
    chkOut: ["inpOutDir", "inpPrefix", "btnBrowseOut"],
  };
  for (const [chk, inputs] of Object.entries(map)) {
    const on = $(chk).checked;
    for (const i of inputs) {
      const el = $(i);
      if (!el) continue;
      el.disabled = !on;
    }
  }
  $("inpStill").disabled = !$("chkType").checked || $("inpType").value !== "still";
  updatePreview();
}

async function runProbe() {
  const blender = $("selBlender").value.trim();
  const blend = $("inpBlend").value.trim();
  const st = $("probeStatus");
  if (!blender || !blend) {
    st.textContent = "先选择 Blender 和 .blend";
    st.className = "status-text err";
    return;
  }
  st.textContent = "读取中…（首次加载场景可能需要几十秒）";
  st.className = "status-text";
  $("btnProbe").disabled = true;
  try {
    const res = await invoke("probe_blend", { blender, blend });
    S.probe = res;
    st.textContent = `Blender ${res.blenderVersion} · ${res.scenes.length} 个场景`;
    st.className = "status-text ok";
    renderScenes();
    if (res.scenes.length) {
      const active = res.scenes.find((s) => s.name === res.activeScene) || res.scenes[0];
      selectScene(active);
    }
    if (S.settings) {
      S.settings.lastBlender = blender;
      S.settings.lastBlendDir = dirOf(blend);
      invoke("set_settings", { settings: S.settings });
    }
  } catch (e) {
    st.textContent = "读取失败";
    st.className = "status-text err";
    toast(String(e).slice(0, 300), "err");
    log(String(e), "error");
  } finally {
    $("btnProbe").disabled = false;
  }
}

function renderScenes() {
  const box = $("sceneList");
  box.innerHTML = "";
  if (!S.probe || !S.probe.scenes.length) {
    box.innerHTML = `<div class="dim small">没有读到场景</div>`;
    return;
  }
  for (const sc of S.probe.scenes) {
    const item = document.createElement("label");
    item.className = "scene-item";
    const r = document.createElement("input");
    r.type = "radio";
    r.name = "scene";
    r.onchange = () => selectScene(sc);
    const title = document.createElement("div");
    title.innerHTML = `<b>${esc(sc.name)}</b>`;
    const meta = document.createElement("div");
    meta.className = "meta";
    meta.innerHTML = `<span>${esc(sc.engine)}</span><span>${sc.frameStart}–${sc.frameEnd}</span>
      <span>${sc.resolution[0]}×${sc.resolution[1]} @${sc.resolutionPercentage}%</span>
      <span>${sc.fps} fps</span><span>${esc(sc.format)}</span>
      ${sc.templateOk ? "" : '<span style="color:#ff8b84">模板不可用</span>'}`;
    const metaWrap = document.createElement("div");
    metaWrap.appendChild(title);
    metaWrap.appendChild(meta);
    item.appendChild(r);
    item.appendChild(metaWrap);
    box.appendChild(item);
    item._scene = sc;
    item._radio = r;
  }
}

function selectScene(sc, existing) {
  S.scene = sc;
  document.querySelectorAll(".scene-item").forEach((el) => {
    const on = el._scene === sc;
    el.classList.toggle("on", on);
    if (el._radio) el._radio.checked = on;
  });

  // 相机
  const cam = $("inpCamera");
  cam.innerHTML = "";
  for (const c of sc.cameras) {
    const o = document.createElement("option");
    o.value = c;
    o.textContent = c;
    cam.appendChild(o);
  }
  if (sc.activeCamera) cam.value = sc.activeCamera;

  $("inpResW").value = sc.resolution[0];
  $("inpResH").value = sc.resolution[1];
  $("inpResPct").value = sc.resolutionPercentage;
  $("inpFps").value = sc.fps;
  $("inpStart").value = sc.frameStart;
  $("inpEnd").value = sc.frameEnd;
  $("inpStep").value = sc.frameStep;
  $("inpStill").value = sc.frameStart;
  $("inpType").value = "anim";

  const fmt = $("inpFormat");
  if ([...fmt.options].some((o) => o.value === sc.format)) fmt.value = sc.format;
  else if (MOVIE.has(sc.format)) fmt.value = "FFMPEG";

  $("inpOutDir").value = dirOf(sc.pathPrefix);
  $("inpPrefix").value = "frame_";

  if (existing) {
    restoreOverrides(existing);
  }
  syncOverrideInputs();
}

function restoreOverrides(job) {
  const ov = job.overrides || {};
  if (ov.camera) { $("chkCamera").checked = true; $("inpCamera").value = ov.camera; }
  if (ov.resolution) {
    $("chkRes").checked = true;
    $("inpResW").value = ov.resolution[0];
    $("inpResH").value = ov.resolution[1];
  }
  if (ov.resolutionPercentage) { $("chkRes").checked = true; $("inpResPct").value = ov.resolutionPercentage; }
  if (ov.fps) { $("chkFps").checked = true; $("inpFps").value = ov.fps; }
  if (ov.frameStart != null || ov.frameEnd != null || ov.frameStep != null) {
    $("chkRange").checked = true;
    if (ov.frameStart != null) $("inpStart").value = ov.frameStart;
    if (ov.frameEnd != null) $("inpEnd").value = ov.frameEnd;
    if (ov.frameStep != null) $("inpStep").value = ov.frameStep;
  }
  if (ov.format) { $("chkFormat").checked = true; $("inpFormat").value = ov.format; }
  if (ov.outputDir || ov.prefix) {
    $("chkOut").checked = true;
    if (ov.outputDir) $("inpOutDir").value = ov.outputDir;
    if (ov.prefix) $("inpPrefix").value = ov.prefix;
  }
  if (!job.animation) {
    $("chkType").checked = true;
    $("inpType").value = "still";
    $("inpStill").value = job.stillFrame;
  }
}

/* ------------------------------------------------------------ 覆盖 -> 任务 */

function collectOverrides() {
  const ov = {
    camera: null, resolution: null, resolutionPercentage: null, fps: null,
    frameStart: null, frameEnd: null, frameStep: null,
    format: null, outputDir: null, prefix: null,
  };
  if ($("chkCamera").checked) ov.camera = $("inpCamera").value || null;
  if ($("chkRes").checked) {
    ov.resolution = [parseInt($("inpResW").value) || 0, parseInt($("inpResH").value) || 0];
    ov.resolutionPercentage = parseInt($("inpResPct").value) || 100;
  }
  if ($("chkFps").checked) ov.fps = parseFloat($("inpFps").value) || null;
  if ($("chkRange").checked) {
    ov.frameStart = parseInt($("inpStart").value);
    ov.frameEnd = parseInt($("inpEnd").value);
    ov.frameStep = Math.max(1, parseInt($("inpStep").value) || 1);
  }
  if ($("chkFormat").checked) ov.format = $("inpFormat").value;
  if ($("chkOut").checked) {
    ov.outputDir = $("inpOutDir").value.trim() || null;
    ov.prefix = $("inpPrefix").value.trim() || "frame_";
  }
  return ov;
}

function resolveTemplate(sc, ov) {
  const fmt = ov.format || sc.format;
  if (MOVIE.has(fmt)) {
    return { ok: false, movie: true, prefix: "", pad: 4, suffix: "", preview: "（视频文件，无法帧级续接）" };
  }
  if (!ov.outputDir && !ov.prefix) {
    let suffix = sc.pathSuffix;
    if (ov.format && ov.format !== sc.format) suffix = EXT[ov.format] || suffix;
    const ok = sc.templateOk;
    return {
      ok, movie: false, prefix: sc.pathPrefix, pad: sc.pathPad, suffix,
      preview: ok ? sc.pathPrefix + pad4(sc.frameStart).padStart(sc.pathPad, "0") + suffix : "—",
    };
  }
  const dir = ov.outputDir || dirOf(sc.pathPrefix);
  const pfx = ov.prefix || "frame_";
  const prefix = joinPath(dir, pfx);
  const suffix = EXT[fmt] || "";
  return {
    ok: !!prefix, movie: false, prefix, pad: 4, suffix,
    preview: prefix + pad4(sc.frameStart) + suffix,
  };
}

function currentTemplate() {
  if (!S.scene) return null;
  return resolveTemplate(S.scene, collectOverrides());
}

function updatePreview() {
  const t = currentTemplate();
  if (!t) {
    $("outPreview").textContent = "—";
    $("movieWarn").classList.add("hidden");
    return;
  }
  $("outPreview").textContent = t.preview;
  $("movieWarn").classList.toggle("hidden", !t.movie);
}

async function saveJob() {
  const blender = $("selBlender").value.trim();
  const blend = $("inpBlend").value.trim();
  if (!blender || !blend) return toast("请选择 Blender 与 .blend", "err");
  if (!S.scene) return toast("请先「读取参数」并选择场景", "err");

  const ov = collectOverrides();
  const t = resolveTemplate(S.scene, ov);
  const anim = $("inpType").value !== "still" || !$("chkType").checked;
  const still = parseInt($("inpStill").value) || S.scene.frameStart;

  const start = ov.frameStart != null ? ov.frameStart : S.scene.frameStart;
  const end = ov.frameEnd != null ? ov.frameEnd : S.scene.frameEnd;
  const step = ov.frameStep != null ? ov.frameStep : S.scene.frameStep;

  const job = {
    id: S.editing?.id || "",
    name: S.editing?.name || `${baseOf(blend).replace(/\.blend$/i, "")} · ${S.scene.name}`,
    blender, blend,
    scene: S.scene.name,
    engine: S.scene.engine,
    animation: anim,
    stillFrame: still,
    frameStart: anim ? start : still,
    frameEnd: anim ? end : still,
    frameStep: anim ? Math.max(1, step) : 1,
    pathPrefix: t.prefix,
    pathPad: t.pad,
    pathSuffix: t.suffix,
    templateOk: t.ok,
    isMovie: t.movie,
    cyclesDevice: S.probe?.cyclesDevice || "",
    cyclesDevices: S.probe?.cyclesDevices || [],
    overrides: ov,
    resume: $("chkResume").checked,
    autoRestart: $("chkAutoRestart").checked,
    maxRetries: Math.max(0, parseInt($("inpRetries").value) || 0),
    verify: $("chkVerify").checked,
    status: "queued",
    progress: { total: 0, done: 0, failed: 0, current: 0, percent: 0, restarts: 0, message: "" },
    attempts: {},
    failedFrames: [],
    created: 0,
    elapsed: 0,
  };

  try {
    if (S.editing) {
      await invoke("update_job", { job });
    } else {
      await invoke("add_jobs", { jobs: [job] });
    }
    $("dlgOverlay").classList.add("hidden");
    await refresh();
    log(`已加入队列：${job.name}`, "ok");
    if (!t.ok && !t.movie) {
      log(
        `注意：${job.name} 没有可用的输出路径（.blend 里未设置或指向 /tmp），续接对它无效；建议在该任务里勾选「输出路径」覆盖`,
        "warn"
      );
    }

    // 记住这次用过的 Blender，下次直接可选；首次使用自动设为默认
    S.settings = S.settings || {};
    const hist = new Set(S.settings.blenderPaths || []);
    if (job.blender) hist.add(job.blender);
    S.settings.blenderPaths = [...hist];
    if (job.blender && !S.settings.defaultBlender) S.settings.defaultBlender = job.blender;
    invoke("set_settings", { settings: S.settings }).catch(() => {});
  } catch (e) {
    toast(String(e).slice(0, 300), "err");
  }
}

/* ------------------------------------------------------------ 设置 */

/* ------------------------------------------------------------ 皮肤 */

const SKINS = [
  { id: "01", name: "铣削面板", bg: "#15171a", panel: "#1f2327", acc: "#ffa62b" },
  { id: "05", name: "取景监视器", bg: "#08090a", panel: "#101112", acc: "#ff3b30" },
  { id: "08", name: "示波器", bg: "#040606", panel: "#080b0a", acc: "#4dff9e" },
  { id: "09", name: "数控面板", bg: "#454b4e", panel: "#22272a", acc: "#ff6a13" },
  { id: "10", name: "军用电台", bg: "#353b2c", panel: "#3e4534", acc: "#ff7a18" },
  { id: "24", name: "电子小票", bg: "#d9d4c9", panel: "#fbfaf5", acc: "#c0392b" },
  { id: "27", name: "Blender", bg: "#1d1d1d", panel: "#232323", acc: "#4772b3" },
  { id: "32", name: "手绘马克笔", bg: "#fff8d6", panel: "#fffef2", acc: "#ff5c8a" },
  { id: "33", name: "赛博霓虹", bg: "#05060c", panel: "#0a0d17", acc: "#00f0ff" },
  { id: "34", name: "赛博酸绿", bg: "#060a06", panel: "#0b120b", acc: "#7dff00" },
  { id: "45", name: "GBA DMG", bg: "#9bbc0f", panel: "#8bac0f", acc: "#0f380f" },
  { id: "46", name: "GBA 外壳", bg: "#1b1e24", panel: "#2f343b", acc: "#9bbc0f" },
  { id: "50", name: "发票复联", bg: "#ccd4dc", panel: "#eef3f8", acc: "#b03a48" },
  { id: "58", name: "赛博终端", bg: "#050706", panel: "#080b09", acc: "#4dff9e" },
  { id: "69", name: "CRT 显像管", bg: "#03110b", panel: "#061a11", acc: "#6dffb0" },
];

function applySkin(id) {
  let t = String(id || "09");
  if (!SKINS.some((s) => s.id === t)) t = "09";
  document.documentElement.setAttribute("data-theme", t);
  S.skin = t;
  document.querySelectorAll("#skinGrid .skin").forEach((b) => {
    b.classList.toggle("on", b.dataset.id === t);
  });
}

function renderSkins() {
  const box = $("skinGrid");
  if (!box || box.dataset.done) return;
  box.innerHTML = SKINS.map(
    (s) => `<button class="skin" data-id="${s.id}" title="${s.name}">
      <span class="sk-sw" style="background:${s.bg};border-color:#0006">
        <i style="background:${s.panel}"></i><i style="background:${s.acc}"></i>
      </span>
      <span class="sk-txt"><b>${s.id}</b> ${s.name}</span>
    </button>`
  ).join("");
  box.dataset.done = "1";
  box.querySelectorAll(".skin").forEach((b) => {
    b.onclick = () => applySkin(b.dataset.id);
  });
}

function openSettings() {
  const s = S.settings || {};
  // 每次打开都回到第一个页签
  document.querySelectorAll("#setOverlay .tab").forEach((t, i) => t.classList.toggle("on", i === 0));
  document.querySelectorAll("#setOverlay .tabpane").forEach((p, i) => p.classList.toggle("hidden", i !== 0));
  renderSkins();
  applySkin(s.theme || "09");
  $("setBlender").value = s.defaultBlender || "";
  const st = $("setBlenderStatus");
  if (st) {
    st.textContent = "";
    st.className = "field-hint";
  }
  $("setIdle").value = s.idleTimeoutSec ?? 3600;
  $("setQueueAuto").checked = s.queueAuto ?? true;
  $("setClean").checked = s.cleanBlender ?? false;
  $("setShutdown").checked = s.shutdownWhenDone ?? false;
  $("setRetries").value = s.defaultMaxRetries ?? 3;
  $("setOverlay").classList.remove("hidden");
}

async function saveSettings() {
  const s = S.settings || {};
  s.defaultBlender = $("setBlender").value.trim();
  s.idleTimeoutSec = Math.max(0, parseInt($("setIdle").value) || 0);
  s.queueAuto = $("setQueueAuto").checked;
  s.cleanBlender = $("setClean").checked;
  s.shutdownWhenDone = $("setShutdown").checked;
  s.defaultMaxRetries = Math.max(0, parseInt($("setRetries").value) || 0);
  s.theme = S.skin || "09";
  try {
    await invoke("set_settings", { settings: s });
  } catch (e) {
    toast("保存失败：" + String(e).slice(0, 160), "err");
    return;
  }
  S.settings = s;
  // 让新设的默认路径立刻可用（任务弹窗会合并这个列表）
  if (s.defaultBlender && !S.blenders.includes(s.defaultBlender)) {
    S.blenders.push(s.defaultBlender);
  }
  $("setOverlay").classList.add("hidden");
  toast("设置已保存");
}

/** 点空白处关闭任务弹窗：表单完整就顺手加入队列，不完整就静默关掉 */
function backdropCloseJob() {
  const blend = $("inpBlend").value.trim();
  if (blend && S.scene) {
    saveJob();
  } else {
    $("dlgOverlay").classList.add("hidden");
  }
}

/* ------------------------------------------------------------ 批量导入（拖拽 / 文件夹） */

let importing = false;
let importCancel = false;
let importDone = 0;
let importTotal = 0;

function setImportUI(on) {
  const b = $("btnAddFolder");
  if (!b) return;
  if (on) {
    b.textContent = `停止解析 ${importDone}/${importTotal}`;
    b.classList.add("danger");
  } else {
    b.textContent = "文件夹";
    b.classList.remove("danger");
    importDone = importTotal = 0;
  }
}

function buildJobFromScene(blender, blend, sc, probe) {
  const base = baseOf(blend).replace(/\.blend$/i, "");
  return {
    id: "",
    name: `${base} · ${sc.name}`,
    blender,
    blend,
    scene: sc.name,
    engine: sc.engine,
    animation: true,
    stillFrame: sc.frameStart,
    frameStart: sc.frameStart,
    frameEnd: sc.frameEnd,
    frameStep: Math.max(1, sc.frameStep),
    pathPrefix: sc.pathPrefix,
    pathPad: sc.pathPad,
    pathSuffix: sc.pathSuffix,
    templateOk: sc.templateOk,
    isMovie: sc.isMovie,
    cyclesDevice: probe.cyclesDevice || "",
    cyclesDevices: probe.cyclesDevices || [],
    overrides: {},
    resume: S.settings?.defaultResume ?? true,
    autoRestart: true,
    maxRetries: S.settings?.defaultMaxRetries ?? 3,
    verify: true,
    status: "queued",
    progress: { total: 0, done: 0, failed: 0, current: 0, percent: 0, restarts: 0, message: "" },
    attempts: {},
    failedFrames: [],
    created: 0,
    elapsed: 0,
  };
}

async function importDropped(paths) {
  if (importing) return toast("正在批量解析中，请稍候");
  importing = true;
  importCancel = false;
  try {
    const jsons = paths.filter((p) => p.toLowerCase().endsWith(".json"));
    const rest = paths.filter((p) => !p.toLowerCase().endsWith(".json"));

    for (const j of jsons) {
      try {
        const added = await invoke("import_jobs", { path: j });
        log(`导入任务 ${added.length} 个：${baseOf(j)}`, "ok");
      } catch (e) {
        log(`导入失败 ${baseOf(j)}：${String(e).slice(0, 200)}`, "error");
      }
    }

    if (rest.length) {
      const blender = (S.settings?.defaultBlender || "").trim();
      if (!blender) {
        toast("请先在「设置」里指定默认 Blender", "err");
        log("批量建任务需要默认 Blender，请到「设置」里指定", "error");
        await refresh();
        return;
      }

      log(`扫描 ${rest.length} 个拖入路径…`, "info");
      let blends = [];
      try {
        blends = await invoke("scan_blend_files", { paths: rest, maxDepth: 3 });
      } catch (e) {
        toast("扫描失败：" + String(e).slice(0, 120), "err");
        return;
      }
      if (!blends.length) {
        toast("没找到 .blend 文件", "err");
        await refresh();
        return;
      }

      const jobs = [];
      if (blends.length > 8) {
        log(
          `${blends.length} 个工程要逐个启动 Blender 解析，可能需要几分钟；随时可以点「停止解析」`,
          "warn"
        );
      }
      importTotal = blends.length;
      for (let i = 0; i < blends.length; i++) {
        if (importCancel) {
          log(`已取消解析（完成 ${i}/${blends.length}）`, "warn");
          break;
        }
        importDone = i + 1;
        setImportUI(true);
        const b = blends[i];
        log(`解析 ${i + 1}/${blends.length}：${baseOf(b)}`, "info");
        try {
          const res = await invoke("probe_blend", { blender, blend: b });
          const sc = (res.scenes || []).find((s) => s.name === res.activeScene) || res.scenes[0];
          if (!sc) {
            log(`  ${baseOf(b)} 没有可用场景，跳过`, "warn");
            continue;
          }
          jobs.push(buildJobFromScene(blender, b, sc, res));
          log(
            `  → ${sc.name} · ${sc.frameStart}-${sc.frameEnd} · ${sc.resolution[0]}×${sc.resolution[1]} · ${sc.format}`,
            "dim"
          );
        } catch (e) {
          log(`  解析失败：${String(e).slice(0, 240)}`, "error");
        }
      }

      if (jobs.length) {
        await invoke("add_jobs", { jobs });
        log(`批量加入 ${jobs.length} 个任务`, "ok");
        toast(`已加入 ${jobs.length} 个任务`);
      } else {
        toast("没有成功解析出任务", "err");
      }
    }
    await refresh();
  } finally {
    importing = false;
    setImportUI(false);
  }
}

/* ------------------------------------------------------------ 队列结束 -> 关机 */

let wasBusy = false;
let idleTimer = null;
let offTimer = null;
let offLeft = 0;
let offDry = false;

function queueBusy() {
  return [...S.jobs.values()].some(
    (j) => j.status === "running" || j.status === "queued"
  );
}

/** 队列从"忙"变成"空"时触发；延迟 2.5s 再判断，避开队列自动流转的空档 */
function noteQueueState() {
  if (queueBusy()) {
    wasBusy = true;
    if (idleTimer) {
      clearTimeout(idleTimer);
      idleTimer = null;
    }
    return;
  }
  if (!wasBusy || idleTimer) return;
  idleTimer = setTimeout(() => {
    idleTimer = null;
    if (queueBusy()) return;
    wasBusy = false;
    if (S.settings?.shutdownWhenDone) showShutdownDialog();
  }, 2500);
}

function showShutdownDialog(opts = {}) {
  offDry = !!opts.dry;
  offLeft = 180;
  $("offSec").textContent = offLeft;
  $("offOverlay").classList.remove("hidden");
  // 交给系统自己的定时器：本程序即使被关掉，关机计划依然有效
  if (!offDry) {
    invoke("system_shutdown", { seconds: 180 }).catch((e) =>
      toast("关机命令失败：" + String(e).slice(0, 120), "err")
    );
  }
  if (offTimer) clearInterval(offTimer);
  offTimer = setInterval(() => {
    offLeft -= 1;
    $("offSec").textContent = Math.max(0, offLeft);
    if (offLeft <= 0) {
      clearInterval(offTimer);
      offTimer = null;
      $("offOverlay").classList.add("hidden");
    }
  }, 1000);
  log(`队列已结束，${offLeft} 秒后自动关机（可取消）`, "warn");
}

function cancelShutdown() {
  if (offTimer) {
    clearInterval(offTimer);
    offTimer = null;
  }
  $("offOverlay").classList.add("hidden");
  if (!offDry) invoke("cancel_shutdown").catch(() => {});
  log("已取消自动关机", "ok");
  toast("已取消关机");
}

function shutdownNow() {
  if (offTimer) {
    clearInterval(offTimer);
    offTimer = null;
  }
  $("offOverlay").classList.add("hidden");
  if (!offDry) {
    invoke("system_shutdown", { seconds: 0 }).catch((e) =>
      toast("关机失败：" + String(e).slice(0, 120), "err")
    );
  }
  toast("正在关机…");
}

/* ------------------------------------------------------------ 事件绑定 */

function bind() {
  $("btnAdd").onclick = () => openJobDialog(null);
  $("btnAddFolder").onclick = async () => {
    // 解析过程中这个按钮变成「停止解析」
    if (importing) {
      importCancel = true;
      $("btnAddFolder").textContent = "停止中…";
      return;
    }
    const p = await dlg.open({
      directory: true,
      multiple: true,
      defaultPath: S.settings?.lastBlendDir || undefined,
    });
    if (!p) return;
    const dirs = Array.isArray(p) ? p : [p];
    S.settings = S.settings || {};
    S.settings.lastBlendDir = dirs[0];
    invoke("set_settings", { settings: S.settings }).catch(() => {});
    await importDropped(dirs);
  };
  $("btnStart").onclick = async () => {
    const ids = selOrAll();
    if (!ids.length) return toast("队列是空的", "err");
    await invoke("start_jobs", { ids });
    await refresh();
  };
  $("btnStop").onclick = async () => {
    const ids = selOrAll();
    if (!ids.length) return;
    await invoke("stop_jobs", { ids });
  };
  $("btnRetry").onclick = async () => {
    const ids = selOrAll();
    if (!ids.length) return;
    await invoke("retry_failed", { ids });
    await refresh();
  };
  $("btnRemove").onclick = async () => {
    const ids = selOrAll();
    if (!ids.length) return;
    await invoke("remove_jobs", { ids });
    S.sel.clear();
    await refresh();
  };
  $("btnImport").onclick = async () => {
    const p = await dlg.open({ filters: [{ name: "任务 JSON", extensions: ["json"] }] });
    if (!p) return;
    try {
      const added = await invoke("import_jobs", { path: p });
      await refresh();
      toast(`导入 ${added.length} 个任务`);
    } catch (e) {
      toast(String(e).slice(0, 300), "err");
    }
  };
  $("btnExport").onclick = async () => {
    const ids = selOrAll();
    if (!ids.length) return toast("没有可导出的任务", "err");
    const p = await dlg.save({
      defaultPath: (S.settings?.lastBlendDir || "") + "\\renderguard-jobs.json",
      filters: [{ name: "任务 JSON", extensions: ["json"] }],
    });
    if (!p) return;
    const n = await invoke("export_jobs", { ids, path: p });
    toast(`已导出 ${n} 个任务`);
  };
  $("btnDataDir").onclick = async () => {
    const d = await invoke("data_dir");
    invoke("open_path", { path: d }).catch((e) => toast(String(e), "err"));
  };
  $("btnSettings").onclick = openSettings;
  $("setCancel").onclick = $("setClose").onclick = () => {
    // 取消时把皮肤还原成已保存的
    applySkin((S.settings || {}).theme || "09");
    $("setOverlay").classList.add("hidden");
  };
  $("setSave").onclick = saveSettings;
  $("offClose").onclick = cancelShutdown;
  $("offCancel").onclick = cancelShutdown;
  $("offNow").onclick = shutdownNow;

  // 设置页签
  document.querySelectorAll("#setOverlay .tab").forEach((t) => {
    t.onclick = () => {
      document.querySelectorAll("#setOverlay .tab").forEach((x) =>
        x.classList.toggle("on", x === t)
      );
      document.querySelectorAll("#setOverlay .tabpane").forEach((p) =>
        p.classList.toggle("hidden", p.dataset.pane !== t.dataset.tab)
      );
    };
  });
  $("aboutLink").onclick = (e) => {
    e.preventDefault();
    invoke("open_path", { path: "https://space.bilibili.com/27284213" }).catch((err) =>
      toast(String(err).slice(0, 160), "err")
    );
  };

  // 点子面板外的空白处：设置=保存并关闭；任务=能保存就保存，不能就关掉。
  // 关机倒计时不响应背景点击 —— 破坏性操作只认明确按钮。
  document.querySelectorAll(".overlay").forEach((ov) => {
    ov.addEventListener("mousedown", (e) => {
      if (e.target !== ov) return;
      if (ov.id === "setOverlay") saveSettings();
      else if (ov.id === "dlgOverlay") backdropCloseJob();
    });
  });

  $("btnConsoleClear").onclick = () => {
    $("console").innerHTML = "";
    S.logLines = 0;
    lastLogKey = "";
  };
  $("btnNoise").onclick = () => {
    S.showNoise = !S.showNoise;
    $("btnNoise").classList.toggle("on", S.showNoise);
    toast(S.showNoise ? "已显示 Blender 噪声日志" : "已隐藏 Blender 噪声日志");
  };
  $("btnConsoleLog").onclick = async () => {
    const id = [...S.sel][0];
    if (!id) return toast("先选中一个任务", "err");
    const p = await invoke("log_path", { id });
    invoke("open_path", { path: p }).catch((e) => toast(String(e), "err"));
  };

  // 队列选择
  $("queueBody").addEventListener("click", (e) => {
    const row = e.target.closest(".row");
    if (!row) return;
    const id = row.dataset.id;
    if (e.ctrlKey || e.metaKey) {
      S.sel.has(id) ? S.sel.delete(id) : S.sel.add(id);
    } else if (e.shiftKey && S.order.length) {
      const last = [...S.sel].pop();
      const a = S.order.indexOf(last || id);
      const b = S.order.indexOf(id);
      const [s1, s2] = a < b ? [a, b] : [b, a];
      for (let i = s1; i <= s2; i++) S.sel.add(S.order[i]);
    } else {
      S.sel.clear();
      S.sel.add(id);
    }
    document.querySelectorAll(".row").forEach((r) => r.classList.toggle("sel", S.sel.has(r.dataset.id)));
    renderDetail();
  });

  // 弹窗
  $("dlgClose").onclick = $("dlgCancel").onclick = () => $("dlgOverlay").classList.add("hidden");
  $("btnProbe").onclick = runProbe;
  $("btnRefreshOpen").onclick = () => refreshOpenBlenders();
  $("dlgSave").onclick = saveJob;
  $("btnBrowseBlender").onclick = async () => {
    const p = await dlg.open({ filters: [{ name: "Blender", extensions: ["exe"] }] });
    if (!p) return;
    if (!S.blenders.includes(p)) S.blenders.push(p);
    const sel = $("selBlender");
    const o = document.createElement("option");
    o.value = p;
    o.textContent = p;
    sel.appendChild(o);
    sel.value = p;
  };
  $("btnTestBlender").onclick = async () => {
    const b = $("selBlender").value.trim();
    if (!b) return toast("先选择一个 Blender", "err");
    const st = $("blenderStatus");
    st.textContent = "测试中…";
    st.className = "status-text";
    $("btnTestBlender").disabled = true;
    try {
      const v = await invoke("test_blender", { path: b });
      st.textContent = `✓ ${v} 可用`;
      st.className = "status-text ok";
      log(`Blender 有效性测试通过：${v}`, "ok");
    } catch (e) {
      st.textContent = "✗ 不可用";
      st.className = "status-text err";
      toast(`Blender 不可用：${String(e).slice(0, 160)}`, "err");
      log(`Blender 有效性测试失败：${e}`, "error");
    } finally {
      $("btnTestBlender").disabled = false;
    }
  };
  $("btnRememberBlender").onclick = async () => {
    const b = $("selBlender").value.trim();
    if (!b) return toast("先选择一个 Blender", "err");
    S.settings = S.settings || {};
    S.settings.defaultBlender = b;
    if (!(S.settings.blenderPaths || []).includes(b)) {
      S.settings.blenderPaths = [...(S.settings.blenderPaths || []), b];
    }
    await invoke("set_settings", { settings: S.settings });
    toast("已设为默认 Blender");
  };
  $("setBlenderBrowse").onclick = async () => {
    const p = await dlg.open({ filters: [{ name: "Blender", extensions: ["exe"] }] });
    if (p) $("setBlender").value = p;
  };
  $("setBlenderTest").onclick = async () => {
    const b = $("setBlender").value.trim();
    const st = $("setBlenderStatus");
    if (!b) {
      st.textContent = "先填写或浏览选择一个 blender.exe";
      st.className = "field-hint err";
      return;
    }
    st.textContent = "测试中…";
    st.className = "field-hint";
    $("setBlenderTest").disabled = true;
    try {
      const v = await invoke("test_blender", { path: b });
      st.textContent = `✓ ${v} 可用`;
      st.className = "field-hint ok";
      log(`默认 Blender 有效性测试通过：${v}`, "ok");
    } catch (e) {
      st.textContent = `✗ 不可用：${String(e).split("\n")[0].slice(0, 120)}`;
      st.className = "field-hint err";
      log(`默认 Blender 不可用：${e}`, "error");
    } finally {
      $("setBlenderTest").disabled = false;
    }
  };
  $("btnBrowseBlend").onclick = async () => {
    const p = await dlg.open({
      filters: [{ name: "Blender 工程", extensions: ["blend"] }],
      defaultPath: S.settings?.lastBlendDir || undefined,
    });
    if (!p) return;
    $("inpBlend").value = p;
  };
  $("btnBrowseOut").onclick = async () => {
    const p = await dlg.open({ directory: true });
    if (!p) return;
    $("inpOutDir").value = Array.isArray(p) ? p[0] : p;
    updatePreview();
  };

  ["chkCamera", "chkRes", "chkFps", "chkRange", "chkType", "chkFormat", "chkOut"].forEach((k) => {
    $(k).onchange = syncOverrideInputs;
  });
  ["inpResW", "inpResH", "inpResPct", "inpFps", "inpStart", "inpEnd", "inpStep",
   "inpType", "inpStill", "inpFormat", "inpOutDir", "inpPrefix"].forEach((k) => {
    const el = $(k);
    if (el) el.addEventListener("input", updatePreview);
    if (el) el.addEventListener("change", () => {
      $("inpStill").disabled = !$("chkType").checked || $("inpType").value !== "still";
      updatePreview();
    });
  });
}

/* ------------------------------------------------------------ 启动 */

async function boot() {
  if (!invoke) {
    document.body.innerHTML =
      '<div style="padding:60px;color:#8b95a5;font-size:14px;line-height:2">' +
      "这个界面需要在 RenderGuard 应用里打开。<br>直接双击 renderguard.exe 即可。</div>";
    return;
  }

  fillFormatSelect();
  bind();

  await listen("rg://job", (e) => upsertJob(e.payload));
  await listen("rg://log", (e) => {
    const job = S.jobs.get(e.payload.id);
    const label = job ? job.name : e.payload.id;
    for (const l of e.payload.lines) log(l.text, l.level, label);
  });
  await listen("rg://jobs-changed", () => refresh());

  await listen("tauri://drag-drop", async (e) => {
    const paths = (e.payload && e.payload.paths) || [];
    if (!paths.length) return;
    // 编辑弹窗开着的时候不抢焦点
    if (!$("dlgOverlay").classList.contains("hidden")) return;
    await importDropped(paths);
  });

  try {
    S.blenders = await invoke("detect_blenders");
    S.settings = await invoke("get_settings");
    // 主题要在第一帧就位，避免启动闪一下默认皮肤
    applySkin((S.settings && S.settings.theme) || "09");
    const dd = await invoke("data_dir");
    $("dataDirHint").textContent = dd;
  } catch (e) {
    log("初始化异常：" + e, "error");
  }

  await refresh();
  log("RenderGuard 已就绪 · 队列 " + S.order.length + " 个任务", "ok");
}

boot();
