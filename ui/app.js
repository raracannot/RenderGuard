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
  partial: "部分完成", failed: "失败", stopped: "已停止", blocked: "依赖未满足",
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
  const err = job.errorKind ? `<small class="errkind">${esc(ERROR_KIND[job.errorKind] || "渲染失败")}</small>` : "";
  return `
    <div class="cell grip" title="拖动调整顺序">⠿</div>
    <div class="cell onbox"><input type="checkbox" data-on="${esc(job.id)}" ${job.enabled === false ? "" : "checked"} title="勾选才会渲染" /></div>
    <div class="cell name" title="${esc(job.name)}">${esc(job.name)}${sub}${err}</div>
    <div class="cell" title="${esc(job.scene)}">${esc(job.scene)}</div>
    <div class="cell mono">${range}</div>
    <div class="cell">${esc((job.engine || "").replace("CYCLES", "Cycles").replace("BLENDER_EEVEE_NEXT", "EEVEE").replace("BLENDER_EEVEE", "EEVEE"))}</div>
    <div class="cell"><span class="chip ${job.status}">${STATUS[job.status] || job.status}</span></div>
    <div class="cell">${progressCell(job)}</div>
    <div class="cell mono">${cur}</div>
    <div class="cell mono">${retry}</div>`;
}

const ERROR_KIND = {
  output: "输出目录不可用",
  format: "格式不支持",
  permission: "权限被拒",
  disk: "磁盘空间不足",
  "missing-scene": "工程/贴图找不到",
  engine: "渲染引擎不可用",
  timeout: "渲染超时/卡死",
  crash: "Blender 崩溃",
  unknown: "渲染失败",
};

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
    row.className = "row" + (S.sel.has(id) ? " sel" : "") + (job.enabled === false ? " off" : "");
    row.dataset.id = id;
    row.innerHTML = rowHTML(job);
    body.appendChild(row);
  }
  $("queueEmpty").classList.toggle("hidden", S.order.length > 0);
  $("queueCount").textContent = String(S.order.length);
  const all = $("chkAllOn");
  if (all) {
    const live = S.order.filter((id) => S.jobs.get(id));
    const on = live.filter((id) => S.jobs.get(id).enabled !== false);
    all.checked = live.length > 0 && on.length === live.length;
    all.indeterminate = on.length > 0 && on.length < live.length;
  }
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
  row.classList.toggle("off", job.enabled === false);
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
  const avg = job.avgFrameSec ? `${job.avgFrameSec.toFixed(1)} 秒/帧` : "—";
  const slow = job.slowestSec
    ? `第 ${job.slowestFrame} 帧 · ${job.slowestSec.toFixed(1)} 秒`
    : "—";
  box.innerHTML = `
    <h3>${esc(job.name)}</h3>
    <div class="thumb" id="thumbBox"><span class="dim small">缩略图加载中…</span></div>
    <dl class="kv">
      <dt>状态</dt><dd><span class="chip ${job.status}">${STATUS[job.status] || job.status}</span></dd>
      ${job.errorKind ? `<dt>失败原因</dt><dd class="errkind">${esc(ERROR_KIND[job.errorKind] || "渲染失败")}</dd>` : ""}
      <dt>场景</dt><dd>${esc(job.scene)}</dd>
      <dt>帧范围</dt><dd>${job.animation ? `${job.frameStart} – ${job.frameEnd}（步 ${job.frameStep}）` : `静帧 ${job.stillFrame}`}</dd>
      <dt>进度</dt><dd>${p.done || 0} / ${p.total || 0} 帧${p.failed ? `，失败 ${p.failed}` : ""}</dd>
      <dt>用时</dt><dd>${fmtSec(job.elapsed)}</dd>
      <dt>平均</dt><dd>${avg}</dd>
      <dt>最慢帧</dt><dd>${slow}</dd>
      <dt>重启</dt><dd>${p.restarts || 0} 次</dd>
      <dt>输出</dt><dd>${esc(outDir)}</dd>
      <dt>工程</dt><dd>${esc(job.blend)}</dd>
      <dt>Blender</dt><dd>${esc(job.blender)}</dd>
      <dt>提示</dt><dd>${esc(p.message || "—")}</dd>
    </dl>
    ${failed ? `<div class="fails"><div class="dim small">失败帧（已隔离）</div><div class="list">${failed}</div></div>` : ""}
    ${(job.missingFiles || []).length ? `<div class="warn-text" style="margin-top:10px">工程里有 ${job.missingFiles.length} 个文件找不到：<br>${esc(job.missingFiles.slice(0, 5).join("\n"))}</div>` : ""}
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

  // 缩略图（只在详情面板打开时取一张，避免一次拉太多）
  const tb = $("thumbBox");
  if (tb) {
    invoke("job_thumb", { id: job.id })
      .then((data) => {
        tb.innerHTML = data
          ? `<img alt="预览" src="${data}">`
          : `<span class="dim small">还没有可预览的成品</span>`;
      })
      .catch(() => {
        tb.innerHTML = `<span class="dim small">缩略图不可用</span>`;
      });
  }
}

/* ------------------------------------------------------------ 本地提醒 */

function beep(freq, times) {
  try {
    const Ctx = window.AudioContext || window.webkitAudioContext;
    if (!Ctx) return;
    const ctx = new Ctx();
    for (let i = 0; i < times; i += 1) {
      const o = ctx.createOscillator();
      const g = ctx.createGain();
      o.type = "sine";
      o.frequency.value = freq;
      g.gain.value = 0.05;
      o.connect(g).connect(ctx.destination);
      const t0 = ctx.currentTime + i * 0.22;
      o.start(t0);
      o.stop(t0 + 0.14);
    }
    setTimeout(() => ctx.close().catch(() => {}), 1200);
  } catch (_) {
    /* 没有音频设备就静默 */
  }
}

function showNotice(n) {
  const box = $("noticeBox");
  if (!box) return;
  const el = document.createElement("div");
  el.className = `notice ${n.level === "error" ? "err" : "ok"}`;
  el.innerHTML = `<b>${esc(n.title || "")}</b><span>${esc(n.body || "")}</span><button class="ghost small" title="关闭">✕</button>`;
  el.querySelector("button").onclick = () => el.remove();
  box.appendChild(el);
  setTimeout(() => el.remove(), 15000);
  beep(n.level === "error" ? 520 : 720, n.level === "error" ? 2 : 1);
}

function fmtSec(s) {  s = Math.floor(s || 0);
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

  ["chkCamera", "chkRes", "chkFps", "chkRange", "chkFormat", "chkOut"].forEach((k) => {
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
  $("inpType").value = "still";
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
  // 渲染类型不是“覆盖项”，始终可选；只有静帧才需要帧号
  $("inpStill").disabled = $("inpType").value !== "still";
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
  const missing = (sc.missingFiles || []).length;
  if (missing) {
    toast(`工程里有 ${missing} 个文件找不到（贴图/库），渲染结果可能缺东西`, "err");
  }
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
  // 默认按“单帧静帧”来，帧号取工程里的当前活动帧
  $("inpStill").value = sc.frameCurrent != null ? sc.frameCurrent : sc.frameStart;
  $("inpType").value = "still";

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
    $("inpType").value = "still";
    $("inpStill").value = job.stillFrame;
  } else {
    $("inpType").value = "anim";
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
  const anim = $("inpType").value === "anim";
  const still =
    parseInt($("inpStill").value, 10) ||
    (S.scene.frameCurrent != null ? S.scene.frameCurrent : S.scene.frameStart);

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
    enabled: true,
    missingFiles: S.scene.missingFiles || [],
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
  $("setMaxConcurrent").value = s.maxConcurrent ?? 1;
  $("setNotifyDone").checked = s.notifyDone ?? false;
  $("setNotifyFail").checked = s.notifyFail ?? false;
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
  s.maxConcurrent = Math.min(8, Math.max(1, parseInt($("setMaxConcurrent").value) || 1));
  s.notifyDone = $("setNotifyDone").checked;
  s.notifyFail = $("setNotifyFail").checked;
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
  $("btnStartQueue").onclick = async () => {
    const n = await invoke("start_queue");
    await refresh();
    toast(n > 0 ? `已加入调度：${n} 个任务` : "没有需要排队的任务");
  };
  $("btnStartSingle").onclick = async () => {
    const ids = [...S.sel];
    if (!ids.length) return toast("先在队列里选中一个任务", "err");
    if (ids.length > 1) return toast("「仅渲染单项」一次只能选一个任务", "err");
    const n = await invoke("start_single", { ids });
    await refresh();
    toast(n > 0 ? "已加入调度（排在当前任务之后）" : "这个任务已经在跑或已排队");
  };
  // 表头全选：一次性开关所有任务的“是否参与调度”
  $("chkAllOn").addEventListener("change", async (e) => {
    const on = e.target.checked;
    try {
      await invoke("set_all_enabled", { enabled: on });
      for (const j of S.jobs.values()) j.enabled = on;
      renderQueue();
      toast(on ? "已全部启用" : "已全部屏蔽（不会渲染）");
    } catch (err) {
      toast("保存失败：" + String(err).slice(0, 120), "err");
      await refresh();
    }
  });

  // 队列：勾选启用 + 拖拽排序（事件委托，重绘后依然有效）
  const qbody = $("queueBody");
  qbody.addEventListener("change", async (e) => {
    const cb = e.target.closest && e.target.closest("input[data-on]");
    if (!cb) return;
    const id = cb.dataset.on;
    try {
      await invoke("set_enabled", { id, enabled: cb.checked });
      const job = S.jobs.get(id);
      if (job) job.enabled = cb.checked;
      const row = document.querySelector(`.row[data-id="${CSS.escape(id)}"]`);
      if (row) row.classList.toggle("off", !cb.checked);
      toast(cb.checked ? "已启用" : "已屏蔽，不会渲染");
    } catch (err) {
      toast("保存失败：" + String(err).slice(0, 120), "err");
    }
  });

  qbody.addEventListener("pointerdown", (e) => {
    const grip = e.target.closest && e.target.closest(".grip");
    if (!grip) return;
    const row = grip.closest(".row");
    if (!row) return;
    e.preventDefault();
    row.classList.add("dragging");
    const clear = () =>
      document
        .querySelectorAll(".row.drop-before,.row.drop-after")
        .forEach((r) => r.classList.remove("drop-before", "drop-after"));
    const move = (ev) => {
      const el = document.elementFromPoint(ev.clientX, ev.clientY);
      const over = el && el.closest && el.closest(".row");
      clear();
      if (!over || over === row) return;
      const r = over.getBoundingClientRect();
      over.classList.add(ev.clientY > r.top + r.height / 2 ? "drop-after" : "drop-before");
    };
    const up = async () => {
      document.removeEventListener("pointermove", move);
      document.removeEventListener("pointerup", up);
      row.classList.remove("dragging");
      const target = document.querySelector(".row.drop-before,.row.drop-after");
      const after = target && target.classList.contains("drop-after");
      clear();
      if (!target) return;
      const ids = S.order.filter((x) => x !== row.dataset.id);
      let idx = ids.indexOf(target.dataset.id);
      if (idx < 0) return;
      if (after) idx += 1;
      ids.splice(idx, 0, row.dataset.id);
      S.order = ids;
      renderQueue();
      try {
        await invoke("reorder_jobs", { ids });
      } catch (err) {
        toast("顺序保存失败：" + String(err).slice(0, 120), "err");
        await refresh();
      }
    };
    document.addEventListener("pointermove", move);
    document.addEventListener("pointerup", up);
  });

  $("btnStop").onclick = async () => {    const ids = selOrAll();
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

  ["chkCamera", "chkRes", "chkFps", "chkRange", "chkFormat", "chkOut"].forEach((k) => {
    $(k).onchange = syncOverrideInputs;
  });
  ["inpResW", "inpResH", "inpResPct", "inpFps", "inpStart", "inpEnd", "inpStep",
   "inpType", "inpStill", "inpFormat", "inpOutDir", "inpPrefix"].forEach((k) => {
    const el = $(k);
    if (el) el.addEventListener("input", updatePreview);
    if (el) el.addEventListener("change", () => {
      $("inpStill").disabled = $("inpType").value !== "still";
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

  await listen("rg://notice", (e) => showNotice(e.payload || {}));

  await listen("rg://imgproc", (e) => {
    const st = e.payload || {};
    if (st.total) setImgProgress(st.percent || 0, `处理中 ${st.done}/${st.total}`);
    if (!st.running) {
      $("imgRun").disabled = false;
      setImgProgress(100, st.message || "完成");
      toast(st.message || "处理完成");
      srcStat();
    }
  });

  await listen("rg://watermark", (e) => {
    const st = e.payload || {};
    if (st.total && st.running) setImgProgress(st.percent || 0, `水印中 ${st.done}/${st.total}`);
    if (!st.running) {
      $("imgRun").disabled = false;
      setImgProgress(100, st.message || "完成");
      toast(st.message || "水印完成");
    }
  });

  await listen("rg://pack", (e) => {
    const st = e.payload || {};
    setPackProgress(st.percent || 0, st.message || "");
    if (!st.running) {
      const start = $("packStart");
      const cancel = $("packCancel");
      if (start) start.disabled = false;
      if (cancel) cancel.disabled = true;
      if (st.ok) {
        setPackProgress(100, st.message || "已完成");
        toast("打包完成：" + (st.outFile || ""));
      } else {
        toast("打包失败：" + (st.message || "未知错误"), "err");
      }
    }
  });

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

wirePack();
wireRename();
wireImgTool();
boot();

/* ------------------------------------------------------------ 打包视频 */

const PACK_PREF_KEY = "rg.packPrefs";

function loadPackPrefs() {
  try {
    return JSON.parse(localStorage.getItem(PACK_PREF_KEY) || "{}");
  } catch (_) {
    return {};
  }
}

function savePackPrefs() {
  const p = {
    fps: parseInt($("packFps").value, 10) || 30,
    order: $("packOrder").value,
    format: $("packFormat").value,
    codec: $("packCodec").value,
    quality: $("packQuality").value,
    dir: $("packDir").value.trim(),
  };
  try {
    localStorage.setItem(PACK_PREF_KEY, JSON.stringify(p));
  } catch (_) {}
}

function openPack() {
  const p = loadPackPrefs();
  $("packFps").value = p.fps || 30;
  $("packOrder").value = p.order || "name_asc";
  $("packFormat").value = p.format || "MPEG4";
  $("packCodec").value = p.codec || "H264";
  $("packQuality").value = p.quality || "MEDIUM";
  $("packDir").value = p.dir || "";
  $("packAudio").value = "";
  $("packOut").value = "";
  $("packCount").textContent = "选好文件夹后会自动统计帧数";
  setPackProgress(0, "待开始");
  $("packStart").disabled = false;
  $("packCancel").disabled = true;
  $("packOverlay").classList.remove("hidden");
  syncPackCodecUI();
  if (p.dir) probePackDir();
}

function setPackProgress(pct, msg) {
  const box = document.querySelector(".pack-prog");
  if (!box) return;
  const p = Math.max(0, Math.min(100, pct || 0));
  box.style.setProperty("--p", p.toFixed(1));
  const bar = box.querySelector(".bar i");
  if (bar) bar.style.width = p.toFixed(1) + "%";
  $("packMsg").textContent = msg;
}

async function probePackDir() {
  const dir = $("packDir").value.trim();
  if (!dir) return;
  try {
    const r = await invoke("pack_preview", { srcDir: dir });
    $("packCount").textContent = r.count
      ? `${r.count} 张图 · 默认输出 ${r.stem}.mp4`
      : "这个文件夹里没找到图片（png/jpg/tif/bmp/webp/tga）";
    if (!$("packOut").value.trim()) $("packOut").value = r.outFile || "";
  } catch (e) {
    $("packCount").textContent = "读取失败：" + String(e).slice(0, 100);
  }
}

async function startPack() {
  const cfg = {
    srcDir: $("packDir").value.trim(),
    outFile: $("packOut").value.trim(),
    fps: parseInt($("packFps").value, 10) || 30,
    order: $("packOrder").value,
    format: $("packFormat").value,
    codec: $("packCodec").value,
    quality: $("packQuality").value,
    audio: $("packAudio").value.trim(),
    bg: "#0a0a0a",
    rgba: $("packAlpha").checked,
    prores: $("packProres").value,
  };
  if (!cfg.srcDir) return toast("请先选择序列文件夹", "err");
  savePackPrefs();
  $("packStart").disabled = true;
  $("packCancel").disabled = false;
  setPackProgress(0, "启动中…");
  try {
    await invoke("pack_start", { cfg });
  } catch (e) {
    $("packStart").disabled = false;
    $("packCancel").disabled = true;
    toast(String(e).slice(0, 200), "err");
  }
}

function wirePack() {
  $("btnPack").onclick = openPack;
  $("packClose").onclick = () => $("packOverlay").classList.add("hidden");
  $("packBrowseDir").onclick = async () => {
    const p = await dlg.open({ directory: true });
    if (!p) return;
    $("packDir").value = Array.isArray(p) ? p[0] : p;
    $("packOut").value = "";
    probePackDir();
  };
  $("packBrowseAudio").onclick = async () => {
    const p = await dlg.open({
      filters: [
        { name: "音频/视频", extensions: ["mp3", "wav", "flac", "m4a", "aac", "ogg", "mp4", "mov", "mkv"] },
      ],
    });
    if (!p) return;
    $("packAudio").value = Array.isArray(p) ? p[0] : p;
  };
  $("packClearAudio").onclick = () => { $("packAudio").value = ""; };
  $("packBrowseOut").onclick = async () => {
    const p = await dlg.save({
      defaultPath: $("packOut").value || "output.mp4",
      filters: [{ name: "视频", extensions: ["mp4", "mkv", "mov", "webm"] }],
    });
    if (!p) return;
    $("packOut").value = Array.isArray(p) ? p[0] : p;
  };
  $("packStart").onclick = startPack;
  $("packCancel").onclick = async () => {
    await invoke("pack_cancel");
    setPackProgress(0, "正在取消…");
  };
  $("packDir").addEventListener("change", probePackDir);

  // 编解码器联动：ProRes / PNG / QTRLE 只在 MOV 里；换编码器时自动切容器
  const onCodec = () => {
    const c = $("packCodec").value;
    if ((c === "PRORES" || c === "PNG" || c === "QTRLE") && $("packFormat").value !== "QUICKTIME") {
      $("packFormat").value = "QUICKTIME";
    }
    if (c !== "PRORES" && c !== "PNG" && c !== "QTRLE" && c !== "H264" && c !== "H265" && c !== "AV1") {
      $("packFormat").value = "MPEG4";
      $("packCodec").value = "H264";
    }
    syncPackCodecUI();
  };
  $("packCodec").addEventListener("change", onCodec);
  $("packFormat").addEventListener("change", onCodec);
  $("packProres").addEventListener("change", syncPackCodecUI);
}

/* 编码器 / 格式 / alpha 联动 */
function syncPackCodecUI() {
  const fmt = $("packFormat").value;
  const codec = $("packCodec").value;
  const prores = $("packProres");
  const alpha = $("packAlpha");
  if (prores) prores.classList.toggle("hidden", codec !== "PRORES");

  const isProresAlpha = codec === "PRORES" && prores &&
    (prores.value === "4444" || prores.value === "4444_XQ");
  const alphaOk = fmt === "QUICKTIME" &&
    (codec === "PNG" || codec === "QTRLE" || isProresAlpha);
  if (alpha) {
    alpha.disabled = !alphaOk;
    if (!alphaOk) alpha.checked = false;
  }
}

/* ------------------------------------------------------------ 批量重命名 */

const RN_PREF = "rg.renamePrefs";

function rnCfg() {
  return {
    dirs: $("rnDir").value.trim(),
    depth: parseInt($("rnDepth").value, 10) || 0,
    include: $("rnInclude").value.trim(),
    exclude: $("rnExclude").value.trim(),
    exts: $("rnExts").value.trim(),
    sort: $("rnSort").value,
    template: $("rnTemplate").value,
    start: parseInt($("rnStart").value, 10) || 1,
    increment: parseInt($("rnStep").value, 10) || 1,
    padding: parseInt($("rnPad").value, 10),
    zeroPad: $("rnZeroPad").checked,
    randomize: false,
    conflict: $("rnConflict").value,
  };
}

function rnSavePrefs() {
  const c = rnCfg();
  try {
    localStorage.setItem(RN_PREF, JSON.stringify(c));
  } catch (_) {}
}

function rnRestorePrefs() {
  let p = {};
  try {
    p = JSON.parse(localStorage.getItem(RN_PREF) || "{}");
  } catch (_) {}
  $("rnDir").value = p.dirs || "";
  $("rnDepth").value = p.depth ?? 0;
  $("rnInclude").value = p.include || "";
  $("rnExclude").value = p.exclude || "";
  $("rnExts").value = p.exts || "png";
  $("rnSort").value = p.sort || "name_asc";
  $("rnTemplate").value = p.template || "<NUMBER>_<SELF>";
  $("rnStart").value = p.start ?? 1;
  $("rnStep").value = p.increment ?? 1;
  $("rnPad").value = p.padding ?? 4;
  $("rnZeroPad").checked = p.zeroPad !== false;
  $("rnConflict").value = p.conflict || "skip";
}

async function rnRefreshUndo() {
  try {
    const v = await invoke("rename_undo_state");
    $("rnUndo").disabled = v == null;
  } catch (_) {
    $("rnUndo").disabled = true;
  }
}

function rnRender(pv) {
  const box = $("rnList");
  box.innerHTML = "";
  if (!pv || pv.error) {
    box.innerHTML = `<div class="dim small">${esc(pv?.error || "还没有预览")}</div>`;
    $("rnStat").textContent = "";
    return;
  }
  $("rnStat").textContent = `共 ${pv.total} 条` + (pv.bad ? ` · ${pv.bad} 条有冲突` : " · 无冲突");
  const frag = document.createDocumentFragment();
  for (const r of pv.rows) {
    const row = document.createElement("div");
    row.className = "rn-row" + (r.issue ? " bad" : "");
    const from = r.name + r.ext;
    const to = r.newName + r.newExt;
    row.innerHTML = `<span class="rn-from" title="${esc(r.src)}">${esc(from)}</span>
      <span class="rn-arrow">→</span>
      <span class="rn-to">${esc(to)}</span>
      ${r.issue ? `<span class="rn-issue" title="${esc(r.detail)}">${esc(
        { dup: "重名", exists: "已存在", illegal: "非法字符", long: "过长" }[r.issue] || r.issue
      )}</span>` : ""}`;
    frag.appendChild(row);
  }
  box.appendChild(frag);
}

async function rnPreview() {
  const cfg = rnCfg();
  if (!cfg.dirs) return toast("请先选择目录", "err");
  rnSavePrefs();
  try {
    const pv = await invoke("rename_preview", { cfg });
    rnRender(pv);
    if (pv.error) toast(pv.error, "err");
  } catch (e) {
    toast("预览失败：" + String(e).slice(0, 160), "err");
  }
}

async function rnApply() {
  const cfg = rnCfg();
  if (!cfg.dirs) return toast("请先选择目录", "err");
  try {
    const r = await invoke("rename_apply", { cfg });
    toast(`重命名完成：成功 ${r.done} 个` + (r.skipped ? `，跳过 ${r.skipped} 个` : ""));
    await rnRefreshUndo();
    await rnPreview();
    await refresh();
  } catch (e) {
    toast("重命名失败：" + String(e).slice(0, 200), "err");
  }
}

function wireRename() {
  $("btnRename").onclick = () => {
    rnRestorePrefs();
    $("rnList").innerHTML = `<div class="dim small">还没有预览</div>`;
    $("rnStat").textContent = "点「预览」查看";
    $("rnOverlay").classList.remove("hidden");
    rnRefreshUndo();
    if ($("rnDir").value.trim()) rnPreview();
  };
  $("rnClose").onclick = () => $("rnOverlay").classList.add("hidden");
  $("rnBrowse").onclick = async () => {
    const p = await dlg.open({ directory: true });
    if (!p) return;
    const v = Array.isArray(p) ? p[0] : p;
    $("rnDir").value = v;
    await rnPreview();
  };
  $("rnPreview").onclick = rnPreview;
  $("rnApply").onclick = rnApply;
  $("rnUndo").onclick = async () => {
    try {
      const n = await invoke("rename_undo");
      toast(`已撤销，恢复 ${n} 个文件`);
      await rnRefreshUndo();
      await rnPreview();
      await refresh();
    } catch (e) {
      toast("撤销失败：" + String(e).slice(0, 160), "err");
    }
  };
}

/* ------------------------------------------------------------ 图像工具箱 */

let imgTab = "convert";

const IMG_TABS = {
  convert: { run: "开始转换", preview: false },
  compress: { run: "开始压缩", preview: false },
  resize: { run: "开始缩放", preview: false },
  mark: { run: "开始加水印", preview: true },
};

function imgSource() {
  return {
    dirs: $("srcDir").value.trim(),
    depth: parseInt($("srcDepth").value, 10) || 0,
    exts: $("srcExts").value.trim(),
  };
}

/** 四个标签页共用一套 ProcCfg，只开当前页需要的那几项 */
function imgProcCfg(tab) {
  const s = imgSource();
  const base = {
    ...s,
    outDir: "",
    suffix: "",
    format: "keep",
    quality: 90,
    targetKb: 0,
    resize: "none",
    percent: 100,
    longEdge: 1920,
    width: 1920,
    height: 1080,
    filter: "lanczos3",
    onlyShrink: true,
    even: true,
    alpha: "white",
    threads: 0,
  };

  if (tab === "convert") {
    base.format = $("cvFormat").value;
    base.quality = parseInt($("cvQuality").value, 10) || 90;
    base.alpha = $("cvAlpha").value;
    base.outDir = $("cvOutDir").value.trim();
    base.suffix = $("cvSuffix").value.trim();
    if (!base.outDir) base.outDir = "";
  } else if (tab === "compress") {
    base.format = $("cpFormat").value;
    base.alpha = $("cpAlpha").value;
    base.outDir = $("cpOutDir").value.trim();
    base.suffix = $("cpSuffix").value.trim();
    if ($("cpMode").value === "target" && base.format === "jpg") {
      base.targetKb = parseInt($("cpTargetKb").value, 10) || 0;
    } else {
      base.quality = parseInt($("cpQuality").value, 10) || 80;
    }
  } else if (tab === "resize") {
    base.resize = $("rsMode").value;
    base.percent = parseFloat($("rsPercent").value) || 100;
    base.longEdge = parseInt($("rsLongEdge").value, 10) || 1920;
    base.width = parseInt($("rsW").value, 10) || 1920;
    base.height = parseInt($("rsH").value, 10) || 1080;
    base.filter = $("rsFilter").value;
    base.onlyShrink = $("rsOnlyShrink").checked;
    base.even = $("rsEven").checked;
    base.format = $("rsFormat").value;
    base.quality = parseInt($("rsQuality").value, 10) || 92;
    base.outDir = $("rsOutDir").value.trim();
    base.suffix = $("rsSuffix").value.trim();
  }
  return base;
}

function wmCfg() {
  const s = imgSource();
  return {
    ...s,
    outDir: "",
    suffix: "_wm",
    kind: $("wmKind").value,
    text: $("wmText").value,
    fontPercent: parseFloat($("wmSize").value) || 5,
    color: $("wmColor").value || "#ffffff",
    opacity: (parseFloat($("wmOpacity").value) || 55) / 100,
    image: $("wmImage").value.trim(),
    imagePercent: parseFloat($("wmSize").value) || 12,
    mode: $("wmMode").value,
    pos: $("wmPos").value,
    offsetX: parseInt($("wmOffX").value, 10) || 0,
    offsetY: parseInt($("wmOffY").value, 10) || 0,
    gapX: parseInt($("wmGapX").value, 10) || 0,
    gapY: parseInt($("wmGapY").value, 10) || 0,
    outFormat: "png",
    quality: 92,
    threads: 0,
  };
}

function rsSyncUI() {
  const m = $("rsMode").value;
  $("rsLongEdge").style.display = m === "long_edge" ? "" : "none";
  $("rsPercent").style.display = m === "percent" ? "" : "none";
  $("rsPercentUnit").style.display = m === "percent" ? "" : "none";
  $("rsW").style.display = m === "exact" ? "" : "none";
  $("rsTimes").style.display = m === "exact" ? "" : "none";
  $("rsH").style.display = m === "exact" ? "" : "none";
}

function cpSyncUI() {
  const byTarget = $("cpMode").value === "target";
  $("cpTargetLabel").style.display = byTarget ? "" : "none";
  $("cpTargetKb").style.display = byTarget ? "" : "none";
  $("cpQualityLabel").style.display = byTarget ? "none" : "";
  $("cpQuality").style.display = byTarget ? "none" : "";
  const isPng = $("cpFormat").value === "png";
  $("cpTargetKb").disabled = isPng;
  if (isPng && byTarget) {
    $("cpMode").value = "quality";
    cpSyncUI();
  }
}

function wmSyncUI() {
  const isImg = $("wmKind").value === "image";
  $("wmText").style.display = isImg ? "none" : "";
  $("wmImage").style.display = isImg ? "" : "none";
  $("wmPickImage").style.display = isImg ? "" : "none";
  $("wmColor").style.display = isImg ? "none" : "";
  $("wmGapRow").style.display = $("wmMode").value === "tile" ? "" : "none";
  $("wmPos").disabled = $("wmMode").value === "tile";
}

function switchImgTab(tab) {
  imgTab = tab;
  document.querySelectorAll("#imgTabs .tab").forEach((b) => {
    b.classList.toggle("on", b.dataset.tab === tab);
  });
  ["convert", "compress", "resize", "mark"].forEach((t) => {
    const pane = { convert: "paneConvert", compress: "paneCompress", resize: "paneResize", mark: "paneMark" }[t];
    $(pane).classList.toggle("hidden", t !== tab);
  });
  $("imgRun").textContent = IMG_TABS[tab].run;
  $("wmPreviewBtn").style.display = IMG_TABS[tab].preview ? "" : "none";
}

function setImgProgress(pct, msg) {
  const box = document.querySelector(".ip-prog");
  if (!box) return;
  box.style.setProperty("--p", (pct || 0).toFixed(1));
  const bar = box.querySelector(".bar i");
  if (bar) bar.style.width = (pct || 0).toFixed(1) + "%";
  $("imgMsg").textContent = msg;
}

async function srcStat() {
  const cfg = imgProcCfg(imgTab === "mark" ? "convert" : imgTab);
  if (!cfg.dirs) {
    $("srcStat").textContent = "";
    return;
  }
  try {
    const s = await invoke("imgproc_stat", { cfg });
    $("srcStat").textContent = s.count
      ? `· ${s.count} 张 / ${(s.bytes / 1048576).toFixed(1)} MB`
      : "· 没找到图片";
  } catch (_) {
    $("srcStat").textContent = "";
  }
}

function openImgTool(tab) {
  $("imgOverlay").classList.remove("hidden");
  switchImgTab(tab || "convert");
  rsSyncUI();
  cpSyncUI();
  wmSyncUI();
  setImgProgress(0, "待开始");
  $("imgRun").disabled = false;
  srcStat();
}

function wireImgTool() {
  $("btnImgTool").onclick = () => openImgTool("convert");
  $("imgClose").onclick = $("imgCancel").onclick = () => $("imgOverlay").classList.add("hidden");
  document.querySelectorAll("#imgTabs .tab").forEach((b) => {
    b.onclick = () => switchImgTab(b.dataset.tab);
  });

  $("srcBrowse").onclick = async () => {
    const p = await dlg.open({ directory: true });
    if (!p) return;
    $("srcDir").value = Array.isArray(p) ? p[0] : p;
    await srcStat();
  };
  ["srcDir", "srcExts", "srcDepth"].forEach((k) => {
    const el = $(k);
    if (el) el.addEventListener("change", srcStat);
  });
  $("rsMode").addEventListener("change", rsSyncUI);
  $("cpMode").addEventListener("change", cpSyncUI);
  $("cpFormat").addEventListener("change", cpSyncUI);
  ["wmKind", "wmMode"].forEach((k) => $(k).addEventListener("change", wmSyncUI));

  $("wmPickImage").onclick = async () => {
    const p = await dlg.open({ filters: [{ name: "图片", extensions: ["png", "jpg", "webp"] }] });
    if (!p) return;
    $("wmImage").value = Array.isArray(p) ? p[0] : p;
  };

  $("wmPreviewBtn").onclick = async () => {
    const cfg = wmCfg();
    if (!cfg.dirs) return toast("请先选择目录", "err");
    try {
      const url = await invoke("watermark_preview", { cfg });
      $("wmPreview").innerHTML = `<img src="${url}" alt="预览" />`;
    } catch (e) {
      toast("预览失败：" + String(e).slice(0, 160), "err");
    }
  };

  $("imgRun").onclick = async () => {
    const src = imgSource();
    if (!src.dirs) return toast("请先选择目录", "err");
    $("imgRun").disabled = true;
    setImgProgress(0, "启动中…");
    try {
      if (imgTab === "mark") {
        await invoke("watermark_start", { cfg: wmCfg() });
        toast("已开始加水印");
      } else {
        await invoke("imgproc_start", { cfg: imgProcCfg(imgTab) });
      }
    } catch (e) {
      $("imgRun").disabled = false;
      toast(String(e).slice(0, 200), "err");
      setImgProgress(0, "失败");
    }
  };
}
