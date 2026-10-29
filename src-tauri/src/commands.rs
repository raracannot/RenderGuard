//! 前端可调用的命令层。

use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::Value;
use tauri::{AppHandle, State};

use crate::model::{Job, JobStatus, ProbeResult};
use crate::probe;
use crate::runner;
use crate::store::{self, Settings, Shared};
use crate::winproc;

fn new_id() -> String {
    static N: AtomicU64 = AtomicU64::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    format!("{}-{}", runner::now_secs(), n)
}

/// 队列泵：空闲时取第一个排队任务开跑
pub fn pump(app: &AppHandle, shared: &Shared) {
    let next = {
        let Ok(s) = shared.lock() else { return };
        if !s.running.is_empty() {
            return;
        }
        s.jobs
            .iter()
            .find(|j| j.status == JobStatus::Queued)
            .map(|j| j.id.clone())
    };
    if let Some(id) = next {
        runner::start(Some(app.clone()), shared.clone(), id);
    }
}

// ------------------------------------------------------------------ 探测

#[tauri::command]
pub async fn probe_blend(blender: String, blend: String) -> Result<ProbeResult, String> {
    tauri::async_runtime::spawn_blocking(move || probe::probe(&blender, &blend))
        .await
        .map_err(|e| e.to_string())?
}

/// 快速验证一个 Blender 是否真的能用：启动 → 初始化 Python → 打印版本。
/// 比只检查文件存在可靠得多（缺 DLL、装坏的 portable 版都能查出来）。
#[tauri::command]
pub async fn test_blender(path: String) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || test_blender_blocking(&path))
        .await
        .map_err(|e| e.to_string())?
}

fn test_blender_blocking(path: &str) -> Result<String, String> {
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    let p = Path::new(path);
    if !p.is_file() {
        return Err("文件不存在".into());
    }

    let dir = store::data_dir().join("state");
    let _ = std::fs::create_dir_all(&dir);
    let log_path = dir.join("blender-test.log");
    let f = std::fs::File::create(&log_path).map_err(|e| e.to_string())?;
    let f2 = f.try_clone().map_err(|e| e.to_string())?;

    let mut cmd = Command::new(path);
    cmd.arg("-b")
        .arg("--factory-startup")
        .arg("--python-expr")
        .arg("import bpy; print('RG_OK', bpy.app.version_string)")
        .arg("-noaudio")
        .stdout(Stdio::from(f))
        .stderr(Stdio::from(f2))
        .creation_flags(winproc::no_window());

    let mut child = cmd
        .spawn()
        .map_err(|e| format!("无法启动进程：{}", e))?;
    winproc::attach_to_job(child.id());

    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {
                if Instant::now() > deadline {
                    winproc::kill_tree(child.id());
                    let _ = child.wait();
                    return Err("启动超时（30 秒无响应）".into());
                }
                std::thread::sleep(Duration::from_millis(120));
            }
            Err(e) => return Err(e.to_string()),
        }
    }

    let text = std::fs::read_to_string(&log_path).unwrap_or_default();
    for line in text.lines() {
        if let Some(rest) = line.trim().strip_prefix("RG_OK") {
            let v = rest.trim();
            return Ok(if v.is_empty() {
                "Blender".to_string()
            } else {
                format!("Blender {}", v)
            });
        }
    }
    // 没拿到标记：把最后几行带回去，方便判断是缺 DLL 还是版本不对
    let tail: Vec<&str> = text.lines().rev().take(6).collect();
    let tail = tail.into_iter().rev().collect::<Vec<_>>().join("\n");
    Err(format!("启动失败或版本异常\n{}", tail))
}

// ------------------------------------------------------------------ 队列

#[tauri::command]
pub fn list_jobs(state: State<'_, Shared>) -> Vec<Job> {
    state.lock().map(|s| s.jobs.clone()).unwrap_or_default()
}

#[tauri::command]
pub fn add_jobs(state: State<'_, Shared>, jobs: Vec<Job>) -> Result<Vec<Job>, String> {
    let mut s = state.lock().map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    for mut j in jobs {
        if j.id.trim().is_empty() {
            j.id = new_id();
        }
        j.created = runner::now_secs();
        j.status = JobStatus::Queued;
        j.failed_frames.clear();
        j.attempts.clear();
        j.elapsed = 0.0;
        j.progress = Default::default();
        j.recount();
        s.jobs.push(j.clone());
        out.push(j);
    }
    store::save_jobs(&s.jobs).map_err(|e| e.to_string())?;
    Ok(out)
}

#[tauri::command]
pub fn update_job(state: State<'_, Shared>, job: Job) -> Result<Job, String> {
    let mut s = state.lock().map_err(|e| e.to_string())?;
    if s.running.contains_key(&job.id) {
        return Err("任务正在渲染，无法修改".into());
    }
    let mut next = job;
    next.failed_frames.clear();
    next.attempts.clear();
    next.recount();

    let Some(slot) = s.jobs.iter_mut().find(|j| j.id == next.id) else {
        return Err("任务不存在".into());
    };
    let keep_status = slot.status;
    next.status = keep_status;
    *slot = next.clone();
    store::save_jobs(&s.jobs).map_err(|e| e.to_string())?;
    Ok(next)
}

#[tauri::command]
pub fn remove_jobs(state: State<'_, Shared>, ids: Vec<String>) -> Result<(), String> {
    let mut s = state.lock().map_err(|e| e.to_string())?;
    for id in &ids {
        if let Some(flag) = s.running.get(id) {
            flag.store(true, Ordering::Relaxed);
        }
    }
    s.jobs.retain(|j| !ids.contains(&j.id));
    store::save_jobs(&s.jobs).map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub fn start_jobs(app: AppHandle, state: State<'_, Shared>, ids: Vec<String>) -> Result<(), String> {
    {
        let mut s = state.lock().map_err(|e| e.to_string())?;
        for id in &ids {
            if s.running.contains_key(id) {
                continue;
            }
            if let Some(j) = s.jobs.iter_mut().find(|j| &j.id == id) {
                j.status = JobStatus::Queued;
                j.failed_frames.clear();
                j.attempts.clear();
                j.progress.message.clear();
            }
        }
        store::save_jobs(&s.jobs).map_err(|e| e.to_string())?;
    }
    pump(&app, state.inner());
    Ok(())
}

#[tauri::command]
pub fn stop_jobs(state: State<'_, Shared>, ids: Vec<String>) -> Result<(), String> {
    let mut s = state.lock().map_err(|e| e.to_string())?;
    for id in &ids {
        if let Some(flag) = s.running.get(id) {
            flag.store(true, Ordering::Relaxed);
        } else if let Some(j) = s.jobs.iter_mut().find(|j| &j.id == id) {
            if j.status == JobStatus::Queued {
                j.status = JobStatus::Stopped;
                j.progress.message = "已取消".into();
            }
        }
    }
    store::save_jobs(&s.jobs).map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub fn retry_failed(app: AppHandle, state: State<'_, Shared>, ids: Vec<String>) -> Result<(), String> {
    {
        let mut s = state.lock().map_err(|e| e.to_string())?;
        for id in &ids {
            if s.running.contains_key(id) {
                continue;
            }
            if let Some(j) = s.jobs.iter_mut().find(|j| &j.id == id) {
                j.failed_frames.clear();
                j.attempts.clear();
                j.status = JobStatus::Queued;
                j.recount();
            }
        }
        store::save_jobs(&s.jobs).map_err(|e| e.to_string())?;
    }
    pump(&app, state.inner());
    Ok(())
}

#[tauri::command]
pub fn clear_finished(state: State<'_, Shared>) -> Result<(), String> {
    let mut s = state.lock().map_err(|e| e.to_string())?;
    let running: Vec<String> = s.running.keys().cloned().collect();
    s.jobs.retain(|j| {
        if running.contains(&j.id) {
            return true;
        }
        !matches!(
            j.status,
            JobStatus::Done | JobStatus::Stopped | JobStatus::Failed
        )
    });
    store::save_jobs(&s.jobs).map_err(|e| e.to_string())?;
    Ok(())
}

// ------------------------------------------------------------------ 导入导出

#[tauri::command]
pub fn export_jobs(state: State<'_, Shared>, ids: Vec<String>, path: String) -> Result<usize, String> {
    let s = state.lock().map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    for id in &ids {
        if let Some(j) = s.jobs.iter().find(|j| &j.id == id) {
            let mut c = j.clone();
            c.status = JobStatus::Queued;
            c.progress = Default::default();
            c.attempts.clear();
            c.failed_frames.clear();
            c.elapsed = 0.0;
            out.push(c);
        }
    }
    let payload = serde_json::json!({
        "app": "RenderGuard",
        "schema": 1,
        "jobs": out,
    });
    store::write_json(Path::new(&path), &payload).map_err(|e| e.to_string())?;
    Ok(out.len())
}

#[tauri::command]
pub fn import_jobs(state: State<'_, Shared>, path: String) -> Result<Vec<Job>, String> {
    let bytes = std::fs::read(&path).map_err(|e| format!("读取失败：{}", e))?;
    let value: Value =
        serde_json::from_slice(&bytes).map_err(|e| format!("不是合法 JSON：{}", e))?;
    let raw = if value.is_array() {
        value.clone()
    } else {
        value
            .get("jobs")
            .cloned()
            .ok_or_else(|| "JSON 里没有 jobs 字段".to_string())?
    };
    let parsed: Vec<Job> =
        serde_json::from_value(raw).map_err(|e| format!("任务格式不匹配：{}", e))?;

    let mut s = state.lock().map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    for mut j in parsed {
        j.id = new_id();
        j.created = runner::now_secs();
        j.status = JobStatus::Queued;
        j.failed_frames.clear();
        j.attempts.clear();
        j.elapsed = 0.0;
        j.progress = Default::default();
        j.recount();
        s.jobs.push(j.clone());
        out.push(j);
    }
    store::save_jobs(&s.jobs).map_err(|e| e.to_string())?;
    Ok(out)
}

// ------------------------------------------------------------------ 环境

#[tauri::command]
pub fn detect_blenders(state: State<'_, Shared>) -> Vec<String> {
    let mut found: Vec<String> = Vec::new();
    if let Ok(s) = state.lock() {
        // 用户明确设置的默认路径排在最前
        if !s.settings.default_blender.is_empty() {
            found.push(s.settings.default_blender.clone());
        }
        found.extend(s.settings.blender_paths.iter().cloned());
        if !s.settings.last_blender.is_empty() {
            found.push(s.settings.last_blender.clone());
        }
    }

    if let Ok(out) = std::process::Command::new("where")
        .arg("blender")
        .creation_flags(winproc::no_window())
        .output()
    {
        if out.status.success() {
            for line in String::from_utf8_lossy(&out.stdout).lines() {
                let p = line.trim();
                if p.to_ascii_lowercase().ends_with("blender.exe") {
                    found.push(p.to_string());
                }
            }
        }
    }

    let mut roots: Vec<PathBuf> = vec![
        PathBuf::from(r"C:\Program Files\Blender Foundation"),
        PathBuf::from(r"C:\Program Files"),
    ];
    if let Ok(la) = std::env::var("LOCALAPPDATA") {
        roots.push(PathBuf::from(la).join("Programs"));
    }
    for root in roots {
        let Ok(rd) = std::fs::read_dir(&root) else {
            continue;
        };
        for e in rd.flatten() {
            let p = e.path();
            if !p.is_dir() {
                continue;
            }
            let direct = p.join("blender.exe");
            if direct.is_file() {
                found.push(direct.to_string_lossy().to_string());
                continue;
            }
            if let Ok(rd2) = std::fs::read_dir(&p) {
                for e2 in rd2.flatten() {
                    let exe = e2.path().join("blender.exe");
                    if exe.is_file() {
                        found.push(exe.to_string_lossy().to_string());
                    }
                }
            }
        }
    }

    let mut seen = std::collections::HashSet::new();
    found.retain(|p| Path::new(p).is_file() && seen.insert(p.to_ascii_lowercase()));
    found.sort();
    found
}

#[tauri::command]
pub fn get_settings(state: State<'_, Shared>) -> Settings {
    state
        .lock()
        .map(|s| s.settings.clone())
        .unwrap_or_default()
}

#[tauri::command]
pub fn set_settings(state: State<'_, Shared>, settings: Settings) -> Result<(), String> {
    let mut s = state.lock().map_err(|e| e.to_string())?;
    s.settings = settings;
    store::save_settings(&s.settings).map_err(|e| e.to_string())?;
    Ok(())
}

/// 列出当前开着的 Blender 窗口（标题里带完整 .blend 路径）
#[tauri::command]
pub fn list_open_blenders() -> Vec<crate::openapp::OpenBlender> {
    crate::openapp::list()
}

/// 把一个或多个路径（文件/文件夹）展开成 .blend 列表。
/// 文件夹按深度递归，跳过隐藏目录与缓存目录。
#[tauri::command]
pub fn scan_blend_files(paths: Vec<String>, max_depth: u32) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut budget: usize = 20000;
    let depth_limit = max_depth.clamp(1, 8);

    for p in paths {
        let path = Path::new(&p);
        if path.is_file() {
            if p.to_ascii_lowercase().ends_with(".blend") {
                out.push(p);
            }
            continue;
        }
        if !path.is_dir() {
            continue;
        }
        let mut stack: Vec<(PathBuf, u32)> = vec![(path.to_path_buf(), 0)];
        while let Some((dir, depth)) = stack.pop() {
            if budget == 0 {
                break;
            }
            budget -= 1;
            let Ok(rd) = std::fs::read_dir(&dir) else {
                continue;
            };
            for e in rd.flatten() {
                let name = e.file_name().to_string_lossy().to_ascii_lowercase();
                if name.starts_with('.') {
                    continue;
                }
                let ep = e.path();
                if ep.is_file() {
                    if name.ends_with(".blend") {
                        out.push(ep.to_string_lossy().to_string());
                    }
                } else if ep.is_dir() && depth < depth_limit {
                    if matches!(
                        name.as_str(),
                        "node_modules" | "__pycache__" | "backup" | "backups" | ".git"
                    ) {
                        continue;
                    }
                    stack.push((ep, depth + 1));
                }
            }
        }
    }

    out.sort();
    out.dedup();
    out
}

/// 安排系统关机。用系统自身定时器（而不是我们的计时器），
/// 这样即使本程序中途退出，关机计划依然成立。
#[tauri::command]
pub fn system_shutdown(seconds: u32) -> Result<(), String> {
    let t = seconds.to_string();
    let out = std::process::Command::new("shutdown")
        .args(["/s", "/t", &t, "/c", "RenderGuard：渲染队列已完成"])
        .creation_flags(winproc::no_window())
        .output()
        .map_err(|e| format!("调用 shutdown 失败：{}", e))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

#[tauri::command]
pub fn cancel_shutdown() -> Result<(), String> {
    let out = std::process::Command::new("shutdown")
        .arg("/a")
        .creation_flags(winproc::no_window())
        .output()
        .map_err(|e| format!("调用 shutdown /a 失败：{}", e))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

/// 用这个任务自己的 Blender 打开它的工程，方便临时改东西
#[tauri::command]
pub fn open_in_blender(blender: String, blend: String) -> Result<(), String> {
    if !Path::new(&blender).is_file() {
        return Err(format!("找不到 Blender：{}", blender));
    }
    if !Path::new(&blend).is_file() {
        return Err(format!("找不到工程文件：{}", blend));
    }
    // 走 shell 启动：Blender 是 GUI 程序，直接 CreateProcess 会带出一个多余的控制台窗口
    winproc::shell_exec(&blender, &format!("\"{}\"", blend))
}

#[tauri::command]
pub fn open_path(path: String) -> Result<(), String> {
    // 网址直接交给浏览器
    if path.starts_with("http://") || path.starts_with("https://") {
        return winproc::shell_open(&path);
    }
    // 文件被删掉时退回到它所在的目录，至少还能打开个地方
    let target = if Path::new(&path).exists() {
        path.clone()
    } else {
        Path::new(&path)
            .parent()
            .map(|p| p.to_string_lossy().to_string())
            .filter(|p| Path::new(p).exists())
            .ok_or_else(|| format!("路径不存在：{}", path))?
    };
    winproc::shell_open(&target)
}

#[tauri::command]
pub fn log_path(id: String) -> String {
    store::data_dir()
        .join("logs")
        .join(format!("{}.log", id))
        .to_string_lossy()
        .to_string()
}

#[tauri::command]
pub fn data_dir() -> String {
    store::data_dir().to_string_lossy().to_string()
}
