//! 守护内核：拉起 Blender 工作进程、监控、崩溃后重启、按帧续接。
//!
//! 设计要点：
//! * 续接的判据是**磁盘上的帧文件**，不是内存里的计数 —— 所以断电重启也能接上
//! * 崩溃后重试用的是"跳过已有帧"，因此每次重启天然接着上次的进度
//! * 卡死判定 = 长时间无输出 **且** CPU 时间不增长（GPU 渲染时 CPU 空闲是正常的，
//!   所以两个条件必须同时满足）

use std::collections::VecDeque;
use std::io::{Read, Write};
use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use tauri::{AppHandle, Emitter};

use crate::model::{Job, JobStatus, LogEvent, LogLine};
use crate::store::{self, Shared};
use crate::winproc;

pub fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn emit_job(app: &Option<AppHandle>, job: &Job) {
    if let Some(a) = app {
        let _ = a.emit("rg://job", job.clone());
    }
}

fn emit_log(app: &Option<AppHandle>, id: &str, lines: Vec<LogLine>) {
    if lines.is_empty() {
        return;
    }
    if let Some(a) = app {
        let _ = a.emit(
            "rg://log",
            LogEvent {
                id: id.to_string(),
                lines,
            },
        );
    }
}

fn strip_ansi(s: &str) -> String {
    if !s.contains('\u{1b}') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        if c == '\u{1b}' {
            if it.peek() == Some(&'[') {
                it.next();
                for c2 in it.by_ref() {
                    if c2.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
            continue;
        }
        out.push(c);
    }
    out
}

/// Blender 启动/退出时插件刷的垃圾信息，不该占用控制台注意力
fn is_noise(l: &str) -> bool {
    l.contains("warning keymap")
        || l.contains("addon is installed")
        || l.contains("addon is uninstalled")
        || l.contains("exception in module unregister")
        || l.contains("gesture backups")
        || l.contains("skipping registration in background")
        || l.contains("reading preferences")
        || l.contains("found bundled python")
        || l.contains("keymaps")
        || l.contains("bl_ext.")
        || l.contains("mp7tools")
}

fn classify(line: &str) -> &'static str {
    let l = line.to_ascii_lowercase();
    if l.starts_with("rg_") || l.contains("rg_setup") || l.contains("rg_probe") {
        return if l.contains("failed") || l.contains("error") {
            "error"
        } else {
            "ok"
        };
    }
    if is_noise(&l) {
        return "dim";
    }
    // 采样进度每条都是噪音，帧号已经由 progress 单独跟踪
    if l.contains("| sample ") || l.contains("remaining:") {
        return "dim";
    }
    if l.contains("traceback")
        || l.contains("error")
        || l.contains("exception")
        || l.contains("segmentation fault")
        || l.contains("access violation")
        || l.contains("failed")
    {
        "error"
    } else if l.contains("warning") {
        "warn"
    } else if l.contains("saved:") || l.contains("finished") || l.contains("skipping existing") {
        "ok"
    } else {
        "info"
    }
}

fn parse_frame(line: &str) -> Option<i32> {
    let idx = line.find("Fra:")?;
    let rest = line[idx + 4..].trim_start();
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    digits.parse().ok()
}

// ---------------------------------------------------------------- 生命周期

pub fn start(app: Option<AppHandle>, shared: Shared, id: String) {
    thread::spawn(move || run_job(app, shared, id));
}

fn run_job(app: Option<AppHandle>, shared: Shared, id: String) {
    let stop = match begin(&app, &shared, &id) {        Some(f) => f,
        None => return,
    };

    let started = Instant::now();
    let mut restarts: u32 = 0;
    let mut first_attempt = true;
    let mut no_progress_rounds = 0u32;
    let mut last_done = 0i64;
    let mut fatal: Option<String> = None;

    loop {
        if stop.load(Ordering::Relaxed) {
            break;
        }

        let job = match snapshot(&shared, &id) {
            Some(j) => j,
            None => break,
        };
        if job.status != JobStatus::Running {
            break;
        }

        // 计算下一个连续区段
        let frames = job.frames();
        let todo: Vec<i32> = frames
            .iter()
            .copied()
            .filter(|f| !job.is_done(*f) && !job.failed_frames.contains(f))
            .collect();
        if todo.is_empty() {
            break;
        }
        let step = if job.animation {
            job.frame_step.max(1)
        } else {
            1
        };
        let seg_start = todo[0];
        let mut seg_end = seg_start;
        for f in todo.iter().skip(1) {
            if *f == seg_end + step {
                seg_end = *f;
            } else {
                break;
            }
        }

        // 第一次是否覆盖，由 resume 决定；之后一律跳过，保证进度能累积
        let overwrite = first_attempt && !job.resume;
        first_attempt = false;

        let outcome = run_segment(&app, &shared, &job, seg_start, seg_end, overwrite, &stop);

        if stop.load(Ordering::Relaxed) || matches!(outcome.end, SegEnd::Stopped) {
            break;
        }

        match outcome.end {
            SegEnd::SpawnError(e) => {
                fatal = Some(format!("无法启动 Blender：{}", e));
                break;
            }
            SegEnd::SetupFailed(rc) => {
                fatal = Some(format!(
                    "参数应用失败（{}），请检查覆盖项：相机 / 格式 / 输出路径",
                    winproc::exit_text(rc)
                ));
                break;
            }
            SegEnd::Exited(0) => {
                // 正常结束：清理坏帧，统计进度
                let fresh = snapshot(&shared, &id).unwrap_or_else(|| job.clone());
                let mut bad = Vec::new();
                if fresh.verify {
                    for f in seg_start..=seg_end {
                        if !fresh.is_done(f) {
                            if let Some(p) = fresh.frame_path(f) {
                                if p.exists() {
                                    let _ = std::fs::remove_file(&p);
                                    bad.push(f);
                                }
                            }
                        }
                    }
                }
                if !bad.is_empty() {
                    let lines = vec![LogLine {
                        level: "warn".into(),
                        text: format!("{:?} 输出校验失败，已删除并重排渲染", bad),
                    }];
                    emit_log(&app, &id, lines);
                }

                let done = update_progress(&app, &shared, &id);
                if done <= last_done {
                    no_progress_rounds += 1;
                } else {
                    no_progress_rounds = 0;
                }
                last_done = done;

                if no_progress_rounds >= 3 {
                    // 反复跑完却没有任何进展：把当前这帧判定为坏帧，继续后面的
                    let mut s = match shared.lock() {
                        Ok(s) => s,
                        Err(_) => break,
                    };
                    if let Some(j) = s.jobs.iter_mut().find(|j| j.id == id) {
                        if !j.failed_frames.contains(&seg_start) {
                            j.failed_frames.push(seg_start);
                            j.progress.failed = j.failed_frames.len() as i64;
                        }
                    }
                    let _ = store::save_jobs(&s.jobs);
                    drop(s);
                    emit_log(
                        &app,
                        &id,
                        vec![LogLine {
                            level: "error".into(),
                            text: format!("帧 {} 多次未产生输出，已跳过", seg_start),
                        }],
                    );
                    no_progress_rounds = 0;
                }
                continue;
            }
            SegEnd::Exited(rc) => {
                restarts += 1;
                let at = outcome.current_frame.unwrap_or(seg_start);
                // 崩在帧间时 current 可能还停在已完成帧上，纠正一下
                let at = {
                    let j = snapshot(&shared, &id).unwrap_or_else(|| job.clone());
                    if j.is_done(at) {
                        seg_start
                    } else {
                        at
                    }
                };

                let (auto_restart, max_retries) = {
                    let s = match shared.lock() {
                        Ok(s) => s,
                        Err(_) => break,
                    };
                    let j = s.jobs.iter().find(|j| j.id == id);
                    (
                        j.map(|j| j.auto_restart).unwrap_or(false),
                        j.map(|j| j.max_retries).unwrap_or(3),
                    )
                };

                let attempt = {
                    let mut s = match shared.lock() {
                        Ok(s) => s,
                        Err(_) => break,
                    };
                    let mut n = 0u32;
                    if let Some(j) = s.jobs.iter_mut().find(|j| j.id == id) {
                        let key = at.to_string();
                        let e = j.attempts.entry(key).or_insert(0);
                        *e += 1;
                        n = *e;
                        j.progress.restarts = restarts;
                    }
                    let _ = store::save_jobs(&s.jobs);
                    n
                };

                let reason = winproc::exit_text(rc);
                emit_log(
                    &app,
                    &id,
                    vec![LogLine {
                        level: "error".into(),
                        text: format!(
                            "Blender 异常退出（{}），帧 {}，第 {} 次",
                            reason, at, attempt
                        ),
                    }],
                );

                archive_crash(&id, started);

                if !auto_restart {
                    fatal = Some(format!("渲染中断（{}），未开启自动重试", reason));
                    break;
                }

                if attempt > max_retries {
                    let mut s = match shared.lock() {
                        Ok(s) => s,
                        Err(_) => break,
                    };
                    if let Some(j) = s.jobs.iter_mut().find(|j| j.id == id) {
                        if !j.failed_frames.contains(&at) {
                            j.failed_frames.push(at);
                        }
                        j.progress.failed = j.failed_frames.len() as i64;
                        j.attempts.remove(&at.to_string());
                    }
                    let _ = store::save_jobs(&s.jobs);
                    drop(s);
                    emit_log(
                        &app,
                        &id,
                        vec![LogLine {
                            level: "warn".into(),
                            text: format!(
                                "帧 {} 连续失败 {} 次，已隔离并继续后续帧",
                                at, attempt
                            ),
                        }],
                    );
                    continue;
                }

                // 退避：给显卡驱动回收显存的时间
                let wait = [5u64, 15, 30, 60, 120]
                    .get((attempt as usize).saturating_sub(1))
                    .copied()
                    .unwrap_or(120);
                emit_log(
                    &app,
                    &id,
                    vec![LogLine {
                        level: "warn".into(),
                        text: format!("{} 秒后自动重启并续接…", wait),
                    }],
                );
                sleep_interruptible(Duration::from_secs(wait), &stop);
                continue;
            }
            SegEnd::Hung => {
                restarts += 1;
                emit_log(
                    &app,
                    &id,
                    vec![LogLine {
                        level: "error".into(),
                        text: "渲染进程无响应，已强制结束并重启".into(),
                    }],
                );
                archive_crash(&id, started);
                let at = outcome.current_frame.unwrap_or(seg_start);
                let blocked = {
                    let mut s = match shared.lock() {
                        Ok(s) => s,
                        Err(_) => break,
                    };
                    let mut blocked = false;
                    if let Some(j) = s.jobs.iter_mut().find(|j| j.id == id) {
                        if !j.auto_restart {
                            blocked = true;
                        } else {
                            let e = j.attempts.entry(at.to_string()).or_insert(0);
                            *e += 1;
                            blocked = *e > j.max_retries;
                            if blocked && !j.failed_frames.contains(&at) {
                                j.failed_frames.push(at);
                            }
                        }
                        j.progress.restarts = restarts;
                    }
                    let _ = store::save_jobs(&s.jobs);
                    blocked
                };
                if blocked {
                    fatal = Some("渲染进程反复卡死，已停止".into());
                    break;
                }
                sleep_interruptible(Duration::from_secs(10), &stop);
                continue;
            }
            SegEnd::Stopped => break,
        }
    }

    finalize(&app, &shared, &id, started, fatal);
}

// ---------------------------------------------------------------- 收尾

fn begin(app: &Option<AppHandle>, shared: &Shared, id: &str) -> Option<Arc<AtomicBool>> {
    let mut s = shared.lock().ok()?;
    if s.running.contains_key(id) {
        return None;
    }
    let idx = s.jobs.iter().position(|j| j.id == id)?;
    {
        let j = &mut s.jobs[idx];
        j.status = JobStatus::Running;
        j.failed_frames.clear();
        j.attempts.clear();
        j.elapsed = 0.0;
        j.progress.message = "准备中…".into();
        j.progress.restarts = 0;
        j.recount();
    }
    let flag = Arc::new(AtomicBool::new(false));
    s.running.insert(id.to_string(), flag.clone());
    let snap = s.jobs[idx].clone();
    let _ = store::save_jobs(&s.jobs);
    drop(s);
    emit_job(app, &snap);
    Some(flag)
}

fn finalize(app: &Option<AppHandle>, shared: &Shared, id: &str, started: Instant, fatal: Option<String>) {
    let mut next = None;
    {
        let mut s = match shared.lock() {
            Ok(s) => s,
            Err(_) => return,
        };
        s.running.remove(id);
        if let Some(j) = s.jobs.iter_mut().find(|j| j.id == id) {
            j.elapsed = started.elapsed().as_secs_f64();
            j.recount();
            let all_done = j.progress.done >= j.progress.total && j.progress.total > 0;
            j.status = if let Some(msg) = &fatal {
                j.progress.message = msg.clone();
                if j.progress.done > 0 {
                    JobStatus::Partial
                } else {
                    JobStatus::Failed
                }
            } else if all_done && j.failed_frames.is_empty() {
                j.progress.message = "全部完成".into();
                JobStatus::Done
            } else if j.progress.done > 0 {
                j.progress.message = format!("完成 {} / {} 帧", j.progress.done, j.progress.total);
                JobStatus::Partial
            } else if j.status == JobStatus::Running {
                j.progress.message = "已停止".into();
                JobStatus::Stopped
            } else {
                j.status
            };
            if j.status == JobStatus::Done {
                j.progress.percent = 100.0;
            }
        }
        let snap = s.jobs.iter().find(|j| j.id == id).cloned();
        // 队列自动流转
        if s.settings.queue_auto {
            if let Some(n) = s
                .jobs
                .iter()
                .find(|j| j.status == JobStatus::Queued && !s.running.contains_key(&j.id))
            {
                next = Some(n.id.clone());
            }
        }
        let _ = store::save_jobs(&s.jobs);
        drop(s);
        if let Some(j) = snap {
            emit_job(app, &j);
        }
    }
    if let Some(nid) = next {
        start(app.clone(), shared.clone(), nid);
    }
}

fn snapshot(shared: &Shared, id: &str) -> Option<Job> {
    let s = shared.lock().ok()?;
    s.jobs.iter().find(|j| j.id == id).cloned()
}

fn update_progress(app: &Option<AppHandle>, shared: &Shared, id: &str) -> i64 {
    let mut s = match shared.lock() {
        Ok(s) => s,
        Err(_) => return 0,
    };
    let done = if let Some(j) = s.jobs.iter_mut().find(|j| j.id == id) {
        j.recount();
        j.progress.done
    } else {
        0
    };
    let snap = s.jobs.iter().find(|j| j.id == id).cloned();
    drop(s);
    if let Some(j) = snap {
        emit_job(app, &j);
    }
    done
}

fn sleep_interruptible(d: Duration, stop: &AtomicBool) {
    let step = Duration::from_millis(200);
    let until = Instant::now() + d;
    while Instant::now() < until {
        if stop.load(Ordering::Relaxed) {
            return;
        }
        thread::sleep(step);
    }
}

fn archive_crash(id: &str, since: Instant) {
    let tmp = std::env::temp_dir();
    for name in ["blender.crash.txt", "blender.crash.txt.1"] {
        let src = tmp.join(name);
        if let Ok(meta) = std::fs::metadata(&src) {
            if let Ok(m) = meta.modified() {
                if m.elapsed().unwrap_or(Duration::from_secs(9999)) < since.elapsed() + Duration::from_secs(5) {
                    let dst = store::data_dir()
                        .join("logs")
                        .join(format!("{}-crash-{}.txt", id, now_secs()));
                    let _ = std::fs::copy(&src, &dst);
                }
            }
        }
    }
}

// ---------------------------------------------------------------- 单个渲染区段

enum SegEnd {
    Exited(i32),
    /// Blender 起来了但参数应用失败 —— 重试没有意义，直接终止
    SetupFailed(i32),
    Stopped,
    Hung,
    SpawnError(String),
}

struct SegOutcome {
    end: SegEnd,
    current_frame: Option<i32>,
}

fn run_segment(
    app: &Option<AppHandle>,
    shared: &Shared,
    job: &Job,
    start: i32,
    end: i32,
    overwrite: bool,
    stop: &AtomicBool,
) -> SegOutcome {
    let dir = store::data_dir();
    let (_, boot) = match store::ensure_scripts() {
        Ok(v) => v,
        Err(e) => {
            return SegOutcome {
                end: SegEnd::SpawnError(e.to_string()),
                current_frame: None,
            }
        }
    };

    let job_json_path: PathBuf = dir.join("jobs").join(format!("{}.json", job.id));
    let hb_path = dir.join("state").join(format!("{}.hb.json", job.id));
    let _ = std::fs::remove_file(&hb_path);

    let payload = serde_json::json!({
        "overrides": job.overrides,
        "cyclesDevice": job.cycles_device,
        "cyclesDevices": job.cycles_devices,
    });
    if let Err(e) = store::write_json(&job_json_path, &payload) {
        return SegOutcome {
            end: SegEnd::SpawnError(format!("写入任务参数失败：{}", e)),
            current_frame: None,
        };
    }

    // 纯净模式：不加载任何插件。第三方渲染引擎（Octane / V-Ray…）依赖插件，
    // 这种情况下自动退回普通模式，并把原因写进控制台。
    let clean_enabled = {
        let s = shared.lock().ok();
        s.map(|s| s.settings.clean_blender).unwrap_or(false)
    };
    let engine_builtin = job.engine.is_empty()
        || job.engine == "CYCLES"
        || job.engine.starts_with("BLENDER_");
    let use_clean = clean_enabled && engine_builtin;
    if clean_enabled && !engine_builtin {
        emit_log(
            app,
            &job.id,
            vec![LogLine {
                level: "warn".into(),
                text: format!("{} 是第三方引擎，纯净模式已跳过（它必须加载插件）", job.engine),
            }],
        );
    }

    let mut cmd = Command::new(&job.blender);
    if use_clean {
        cmd.arg("--factory-startup");
    }
    cmd.arg("-b").arg(&job.blend);
    cmd.arg("-S").arg(&job.scene);
    if job.animation {
        cmd.arg("-s").arg(start.to_string());
        cmd.arg("-e").arg(end.to_string());
    }
    // --python 必须在 -a / -f 之前执行
    cmd.arg("--python-exit-code")
        .arg("1")
        .arg("--python")
        .arg(&boot)
        .arg("-noaudio");
    if job.animation {
        cmd.arg("-a");
    } else {
        cmd.arg("-f").arg(job.still_frame.to_string());
    }

    cmd.env("RG_JOB", &job_json_path)
        .env("RG_HEARTBEAT", &hb_path)
        .env("RG_OVERWRITE", if overwrite { "1" } else { "0" })
        .env("RG_CLEAN", if use_clean { "1" } else { "0" })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .creation_flags(winproc::no_window());

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            return SegOutcome {
                end: SegEnd::SpawnError(e.to_string()),
                current_frame: None,
            }
        }
    };
    let pid = child.id();
    winproc::attach_to_job(pid);

    let (tx, rx) = mpsc::channel::<String>();
    let last_out = Arc::new(Mutex::new(Instant::now()));
    if let Some(o) = child.stdout.take() {
        let tx = tx.clone();
        let lo = last_out.clone();
        thread::spawn(move || pump(o, tx, lo));
    }
    if let Some(e) = child.stderr.take() {
        let tx = tx.clone();
        let lo = last_out.clone();
        thread::spawn(move || pump(e, tx, lo));
    }
    drop(tx);

    let timeout = {
        let s = shared.lock().ok();
        s.map(|s| s.settings.idle_timeout_sec).unwrap_or(3600)
    };

    let log_path = dir.join("logs").join(format!("{}.log", job.id));
    // 日志防爆：超过 8MB 轮转一次，否则长时间挂机会写满盘
    if std::fs::metadata(&log_path)
        .map(|m| m.len() > 8 * 1024 * 1024)
        .unwrap_or(false)
    {
        let _ = std::fs::rename(&log_path, log_path.with_extension("log.1"));
    }
    let mut logf = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .ok();

    let mut current: Option<i32> = None;
    let mut cpu_hist: VecDeque<(Instant, f64)> = VecDeque::new();
    let mut pending: Vec<LogLine> = Vec::new();
    let mut last_emit = Instant::now();
    let mut last_recount = Instant::now();
    let mut last_cpu_sample = Instant::now();
    let mut setup_failed = false;

    if let Some(f) = logf.as_mut() {
        let _ = writeln!(
            f,
            "\n===== {} | 帧 {}-{} | overwrite={} =====",
            fmt_stamp(now_secs()),
            start,
            end,
            overwrite
        );
    }

    let end_state = loop {
        // 1) 收日志
        while let Ok(raw) = rx.try_recv() {
            let line = strip_ansi(&raw);
            if line.trim().is_empty() {
                continue;
            }
            if line.contains("RG_SETUP_FAILED") {
                setup_failed = true;
            }
            if let Some(f) = parse_frame(&line) {
                current = Some(f);
            }
            if let Some(f) = logf.as_mut() {
                let _ = writeln!(f, "{}", line);
            }
            pending.push(LogLine {
                level: classify(&line).to_string(),
                text: line,
            });
        }

        if stop.load(Ordering::Relaxed) {
            winproc::kill_tree(pid);
            let _ = child.wait();
            flush(app, job, &mut pending, &mut last_emit);
            break SegEnd::Stopped;
        }

        match child.try_wait() {
            Ok(Some(status)) => {
                // 给读线程一点时间把尾巴吐完
                thread::sleep(Duration::from_millis(120));
                while let Ok(raw) = rx.try_recv() {
                    let line = strip_ansi(&raw);
                    if !line.trim().is_empty() {
                        pending.push(LogLine {
                            level: classify(&line).to_string(),
                            text: line.clone(),
                        });
                        if let Some(f) = logf.as_mut() {
                            let _ = writeln!(f, "{}", line);
                        }
                    }
                }
                flush(app, job, &mut pending, &mut last_emit);
                let code = status.code().unwrap_or(-1);
                if setup_failed && code != 0 {
                    break SegEnd::SetupFailed(code);
                }
                break SegEnd::Exited(code);
            }
            Ok(None) => {}
            Err(e) => {
                winproc::kill_tree(pid);
                break SegEnd::SpawnError(e.to_string());
            }
        }

        // 2) 卡死检测
        if timeout > 0 {
            let hb_age = std::fs::metadata(&hb_path)
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.elapsed().ok())
                .unwrap_or(Duration::from_secs(86_400));
            let out_age = last_out
                .lock()
                .map(|t| t.elapsed())
                .unwrap_or(Duration::from_secs(0));
            let idle = out_age.min(hb_age);
            if idle > Duration::from_secs(timeout) {
                if last_cpu_sample.elapsed() > Duration::from_secs(2) {
                    if let Some(c) = winproc::cpu_seconds(pid) {
                        cpu_hist.push_back((Instant::now(), c));
                        last_cpu_sample = Instant::now();
                    }
                }
                while cpu_hist.len() > 2 {
                    let old = cpu_hist[0].0.elapsed().as_secs();
                    if old > timeout + 10 {
                        cpu_hist.pop_front();
                    } else {
                        break;
                    }
                }
                let now_cpu = winproc::cpu_seconds(pid).unwrap_or(0.0);
                let base_cpu = cpu_hist.front().map(|(_, c)| *c).unwrap_or(now_cpu);
                if now_cpu - base_cpu < 1.0 {
                    emit_log(
                        app,
                        &job.id,
                        vec![LogLine {
                            level: "error".into(),
                            text: format!(
                                "{} 秒无输出且 CPU 无增长，判定卡死",
                                timeout
                            ),
                        }],
                    );
                    winproc::kill_tree(pid);
                    let _ = child.wait();
                    flush(app, job, &mut pending, &mut last_emit);
                    break SegEnd::Hung;
                }
                cpu_hist.clear();
            }
        }

        // 3) 采样 CPU 供下次判定
        if last_cpu_sample.elapsed() > Duration::from_secs(2) {
            if let Some(c) = winproc::cpu_seconds(pid) {
                cpu_hist.push_back((Instant::now(), c));
                if cpu_hist.len() > 120 {
                    cpu_hist.pop_front();
                }
            }
            last_cpu_sample = Instant::now();
        }

        // 4) 推事件（节流，避免 IPC 洪泛）
        flush(app, job, &mut pending, &mut last_emit);

        if last_recount.elapsed() > Duration::from_secs(2) {
            last_recount = Instant::now();
            let _ = update_running(app, shared, &job.id, current);
        }

        thread::sleep(Duration::from_millis(80));
    };

    SegOutcome {
        end: end_state,
        current_frame: current,
    }
}

fn flush(app: &Option<AppHandle>, job: &Job, pending: &mut Vec<LogLine>, last: &mut Instant) {
    if pending.is_empty() || last.elapsed() < Duration::from_millis(150) {
        return;
    }
    *last = Instant::now();
    let lines = std::mem::take(pending);
    emit_log(app, &job.id, lines);
}

fn update_running(app: &Option<AppHandle>, shared: &Shared, id: &str, current: Option<i32>) -> Option<()> {
    let mut s = shared.lock().ok()?;
    let snap = {
        let j = s.jobs.iter_mut().find(|j| j.id == id)?;
        if let Some(c) = current {
            j.progress.current = c as i64;
        }
        j.recount_quick();
        j.clone()
    };
    drop(s);
    emit_job(app, &snap);
    Some(())
}

fn pump<R: Read + Send + 'static>(mut r: R, tx: mpsc::Sender<String>, last: Arc<Mutex<Instant>>) {
    let mut buf = [0u8; 16 * 1024];
    let mut acc: Vec<u8> = Vec::new();
    loop {
        match r.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                if let Ok(mut t) = last.lock() {
                    *t = Instant::now();
                }
                acc.extend_from_slice(&buf[..n]);
                loop {
                    let pos = acc.iter().position(|b| *b == b'\n' || *b == b'\r');
                    let Some(pos) = pos else { break };
                    let line: Vec<u8> = acc.drain(..pos).collect();
                    if !acc.is_empty() {
                        acc.remove(0);
                    }
                    if !line.is_empty() {
                        let _ = tx.send(String::from_utf8_lossy(&line).to_string());
                    }
                }
                if acc.len() > 256 * 1024 {
                    acc.clear();
                }
            }
            Err(_) => break,
        }
    }
    if !acc.is_empty() {
        let _ = tx.send(String::from_utf8_lossy(&acc).to_string());
    }
}

fn fmt_stamp(secs: u64) -> String {
    // 只用于日志分隔，避免引入 chrono
    format!("t={}", secs)
}
