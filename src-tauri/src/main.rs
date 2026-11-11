#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod commands;
mod filecheck;
mod imgtool;
mod media;
mod model;
mod openapp;
mod pack;
mod probe;
mod rename;
mod runner;
mod store;
mod videoinfo;
mod watermark;
mod winproc;

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Deserialize;
use tauri::Emitter;

fn main() {
    store::ensure_dirs().ok();
    store::ensure_scripts().ok();
    // 清掉一天前的 Blender 会话临时目录残留（被强杀时不会自己删）
    store::cleanup_stale_temp();

    let args: Vec<String> = std::env::args().collect();

    if let Some(i) = args.iter().position(|a| a == "--watermark") {
        match args.get(i + 1) {
            Some(path) => {
                let text = std::fs::read_to_string(path).unwrap_or_default();
                match serde_json::from_str::<watermark::WaterCfg>(&text) {
                    Ok(cfg) => match watermark::run(None, &cfg) {
                        Ok((ok, bad)) => {
                            println!("水印完成：成功 {} 个，失败 {} 个", ok, bad);
                            std::process::exit(0);
                        }
                        Err(e) => {
                            eprintln!("失败：{}", e);
                            std::process::exit(1);
                        }
                    },
                    Err(e) => {
                        eprintln!("配置解析失败：{}", e);
                        std::process::exit(2);
                    }
                }
            }
            None => {
                eprintln!("用法: renderguard.exe --watermark <cfg.json>");
                std::process::exit(2);
            }
        }
    }

    if let Some(i) = args.iter().position(|a| a == "--imgproc") {
        match args.get(i + 1) {
            Some(path) => {
                let text = std::fs::read_to_string(path).unwrap_or_default();
                match serde_json::from_str::<imgtool::ProcCfg>(&text) {
                    Ok(cfg) => match imgtool::run(None, &cfg) {
                        Ok(r) => {
                            println!(
                                "完成 {} 个，跳过 {}，失败 {} | 体积 {:.1} MB → {:.1} MB | {:.1} 秒",
                                r.done, r.skipped, r.failed,
                                r.in_bytes as f64 / 1048576.0,
                                r.out_bytes as f64 / 1048576.0,
                                r.elapsed_ms as f64 / 1000.0
                            );
                            std::process::exit(0);
                        }
                        Err(e) => {
                            eprintln!("失败：{}", e);
                            std::process::exit(1);
                        }
                    },
                    Err(e) => {
                        eprintln!("配置解析失败：{}", e);
                        std::process::exit(2);
                    }
                }
            }
            None => {
                eprintln!("用法: renderguard.exe --imgproc <cfg.json>");
                std::process::exit(2);
            }
        }
    }

    if let Some(i) = args.iter().position(|a| a == "--rename") {
        match args.get(i + 1) {
            Some(path) => {
                let text = std::fs::read_to_string(path).unwrap_or_default();
                match serde_json::from_str::<rename::RenameCfg>(&text) {
                    Ok(cfg) => {
                        let pv = rename::preview(&cfg);
                        if !pv.error.is_empty() {
                            eprintln!("错误：{}", pv.error);
                            std::process::exit(1);
                        }
                        println!("共 {} 条，冲突/异常 {} 条", pv.total, pv.bad);
                        for r in pv.rows.iter().take(12) {
                            println!(
                                "  {:<28} -> {}{} {}",
                                r.name, r.new_name, r.new_ext, r.issue
                            );
                        }
                        std::process::exit(0);
                    }
                    Err(e) => {
                        eprintln!("配置解析失败：{}", e);
                        std::process::exit(2);
                    }
                }
            }
            None => {
                eprintln!("用法: renderguard.exe --rename <cfg.json>");
                std::process::exit(2);
            }
        }
    }

    if let Some(i) = args.iter().position(|a| a == "--rename-apply") {
        match args.get(i + 1) {
            Some(path) => {
                let text = std::fs::read_to_string(path).unwrap_or_default();
                let cfg: rename::RenameCfg = serde_json::from_str(&text).unwrap_or_default();
                match rename::apply(&cfg) {
                    Ok((done, skipped, total)) => {
                        println!("重命名完成：成功 {} 个，跳过 {} 个，共 {} 个", done, skipped, total);
                        std::process::exit(0);
                    }
                    Err(e) => {
                        eprintln!("失败：{}", e);
                        std::process::exit(1);
                    }
                }
            }
            None => {
                eprintln!("用法: renderguard.exe --rename-apply <cfg.json>");
                std::process::exit(2);
            }
        }
    }

    if let Some(i) = args.iter().position(|a| a == "--rename-undo") {
        let _ = i;
        match rename::undo_last() {
            Ok(n) => println!("已撤销，恢复 {} 个文件", n),
            Err(e) => eprintln!("撤销失败：{}", e),
        }
        std::process::exit(0);
    }

    if let Some(i) = args.iter().position(|a| a == "--img") {
        match args.get(i + 1) {
            Some(p) => image_bench(p),
            None => eprintln!("用法: renderguard.exe --img <目录或文件>"),
        }
        std::process::exit(0);
    }

    if let Some(i) = args.iter().position(|a| a == "--pack") {
        let Some(path) = args.get(i + 1) else {
            eprintln!("用法: renderguard.exe --pack <config.json>");
            std::process::exit(2);
        };
        match pack_cli(path) {
            Ok(out) => {
                println!("打包完成：{}", out);
                std::process::exit(0);
            }
            Err(e) => {
                eprintln!("打包失败：{}", e);
                std::process::exit(1);
            }
        }
    }

    if let Some(i) = args.iter().position(|a| a == "--enqueue") {        let Some(path) = args.get(i + 1) else {
            eprintln!("用法: renderguard.exe --enqueue <job.json>");
            std::process::exit(2);
        };
        match enqueue_file(path) {
            Ok(id) => {
                println!("已投递任务 {}，程序启动后会自动加入队列", id);
                std::process::exit(0);
            }
            Err(e) => {
                eprintln!("投递失败：{}", e);
                std::process::exit(1);
            }
        }
    }

    if let Some(i) = args.iter().position(|a| a == "--selftest") {
        if let Some(path) = args.get(i + 1) {
            std::process::exit(selftest(path));
        }
        eprintln!("用法: renderguard.exe --selftest <config.json>");
        std::process::exit(2);
    }

    if let Some(i) = args.iter().position(|a| a == "--probe") {
        let blender = args.get(i + 1).cloned().unwrap_or_default();
        let blend = args.get(i + 2).cloned().unwrap_or_default();
        match probe::probe(&blender, &blend) {
            Ok(r) => {
                println!("{}", serde_json::to_string_pretty(&r).unwrap_or_default());
                std::process::exit(0);
            }
            Err(e) => {
                eprintln!("{}", e);
                std::process::exit(1);
            }
        }
    }

    gui();
}

/// 命令行打包：读配置 → 用设置里的 Blender 编码
fn pack_cli(path: &str) -> Result<String, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("读取 {} 失败：{}", path, e))?;
    let cfg: pack::PackCfg =
        serde_json::from_str(&text).map_err(|e| format!("解析打包配置失败：{}", e))?;
    let blender = store::load_settings().default_blender;
    if blender.trim().is_empty() {
        return Err("设置里没有默认 Blender 路径".into());
    }
    pack::run(None, &blender, &cfg)
}

/// 把外部任务文件投递到 inbox；运行中的程序会在 2 秒内收编
fn enqueue_file(path: &str) -> Result<String, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("读取 {} 失败：{}", path, e))?;
    let mut job: model::Job =
        serde_json::from_str(&text).map_err(|e| format!("解析任务文件失败：{}", e))?;
    if job.blend.trim().is_empty() {
        return Err("任务文件里缺少 blend 字段".into());
    }
    if job.id.trim().is_empty() {
        job.id = format!(
            "cli{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis())
                .unwrap_or(0)
        );
    }
    if job.name.trim().is_empty() {
        job.name = std::path::Path::new(&job.blend)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("任务")
            .to_string();
    }
    job.status = model::JobStatus::Queued;
    job.created = runner::now_secs();
    let out = store::inbox_dir().join(format!("{}.json", job.id));
    store::write_json(&out, &job).map_err(|e| e.to_string())?;
    Ok(job.id)
}

fn gui() {
    winproc::init_kill_on_close_job();

    let mut jobs = store::load_jobs();
    let incoming = store::drain_inbox();
    if !incoming.is_empty() {
        jobs.extend(incoming);
        let _ = store::save_jobs(&jobs);
    }
    let data = store::AppData {
        jobs,
        settings: store::load_settings(),
        running: Default::default(),
    };
    let shared: store::Shared = Arc::new(Mutex::new(data));

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(shared.clone())
        .setup(move |app| {
            // WebView2 默认把用户数据写到 %LOCALAPPDATA%，便携版要求数据跟着 exe 走。
            let webview_dir = store::data_dir().join("webview");
            std::fs::create_dir_all(&webview_dir).ok();

            let mut win = tauri::WebviewWindowBuilder::new(
                app,
                "main",
                tauri::WebviewUrl::App("index.html".into()),
            )
            .title("渲染守护 RenderGuard")
            .inner_size(1240.0, 800.0)
            .min_inner_size(980.0, 640.0)
            .center()
            .resizable(true);
            if webview_dir.is_dir() {
                win = win.data_directory(webview_dir);
            }
            win.build()?;

            let handle = app.handle().clone();
            let shared2 = shared.clone();
            tauri::async_runtime::spawn(async move {
                let mut changed = false;
                {
                    let Ok(mut s) = shared2.lock() else { return };
                    for j in s.jobs.iter_mut() {
                        if j.status == model::JobStatus::Running {
                            j.status = model::JobStatus::Stopped;
                            j.progress.message = "上次被中断，可续接".into();
                            changed = true;
                        }
                    }
                    if changed {
                        let _ = store::save_jobs(&s.jobs);
                    }
                }
                if changed {
                    let _ = handle.emit("rg://jobs-changed", ());
                }

                // 启动后自动接管队列（只对排队中的任务生效，受 queueAuto / 并发上限约束）
                let h = handle.clone();
                let s = shared2.clone();
                std::thread::spawn(move || {
                    std::thread::sleep(std::time::Duration::from_millis(800));
                    crate::runner::schedule_next(&Some(h), &s);
                });
            });

            // inbox 守护：外部 CLI 投递的任务每 2 秒收编一次
            let handle_in = app.handle().clone();
            let shared_in = shared.clone();
            std::thread::spawn(move || loop {
                std::thread::sleep(std::time::Duration::from_secs(2));
                let incoming = store::drain_inbox();
                if incoming.is_empty() {
                    continue;
                }
                {
                    let Ok(mut s) = shared_in.lock() else { continue };
                    for mut j in incoming {
                        if j.id.trim().is_empty() {
                            j.id = format!("in{}", runner::now_secs());
                        }
                        j.status = model::JobStatus::Queued;
                        j.created = runner::now_secs();
                        if j.name.trim().is_empty() {
                            j.name = std::path::Path::new(&j.blend)
                                .file_stem()
                                .and_then(|x| x.to_str())
                                .unwrap_or("任务")
                                .to_string();
                        }
                        s.jobs.push(j);
                    }
                    let _ = store::save_jobs(&s.jobs);
                }
                let _ = handle_in.emit("rg://jobs-changed", ());
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::probe_blend,
            commands::test_blender,
            commands::list_open_blenders,
            commands::list_jobs,
            commands::add_jobs,
            commands::update_job,
            commands::remove_jobs,
            commands::start_jobs,
            commands::stop_jobs,
            commands::retry_failed,
            commands::clear_finished,
            commands::export_jobs,
            commands::import_jobs,
            commands::scan_blend_files,
            commands::detect_blenders,
            commands::get_settings,
            commands::set_settings,
            commands::open_path,
            commands::open_in_blender,
            commands::system_shutdown,
            commands::cancel_shutdown,
            commands::log_path,
            commands::data_dir,
            commands::job_thumb,
            commands::reorder_jobs,
            commands::set_enabled,
            commands::start_queue,
            commands::start_single,
            commands::set_all_enabled,
            commands::pack_start,
            commands::pack_cancel,
            commands::pack_preview,
            commands::rename_preview,
            commands::rename_apply,
            commands::rename_undo,
            commands::rename_undo_state,
            commands::imgproc_start,
            commands::imgproc_stat,
            commands::watermark_start,
            commands::watermark_preview,
        ])
        .run(tauri::generate_context!())
        .expect("RenderGuard 启动失败");
}

// ------------------------------------------------------------------ 自检模式

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct SelfTestCfg {
    blender: String,
    blend: String,
    scene: String,
    frame_start: i32,
    frame_end: i32,
    frame_step: i32,
    animation: bool,
    path_prefix: String,
    path_pad: u32,
    path_suffix: String,
    template_ok: bool,
    is_movie: bool,
    clean: bool,
    resume: bool,
    auto_restart: bool,
    max_retries: u32,
    verify: bool,
    overrides: model::Overrides,
    timeout_sec: u64,
    result_path: String,
}

impl Default for SelfTestCfg {
    fn default() -> Self {
        SelfTestCfg {
            blender: String::new(),
            blend: String::new(),
            scene: "Scene".into(),
            frame_start: 1,
            frame_end: 1,
            frame_step: 1,
            animation: true,
            path_prefix: String::new(),
            path_pad: 4,
            path_suffix: String::new(),
            template_ok: false,
            is_movie: false,
            clean: false,
            resume: true,
            auto_restart: true,
            max_retries: 3,
            verify: true,
            overrides: Default::default(),
            timeout_sec: 300,
            result_path: String::new(),
        }
    }
}

/// 无界面跑一个任务，用于验证"崩溃→自动重启→续接"整条链路。
fn selftest(cfg_path: &str) -> i32 {
    store::ensure_dirs().ok();
    store::ensure_scripts().ok();

    let text = match std::fs::read_to_string(cfg_path) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("读取配置失败: {}", e);
            return 2;
        }
    };
    let cfg: SelfTestCfg = match serde_json::from_str(&text) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("配置解析失败: {}", e);
            return 2;
        }
    };

    let mut job = model::Job {
        id: "selftest".into(),
        name: "selftest".into(),
        blender: cfg.blender.clone(),
        blend: cfg.blend.clone(),
        scene: cfg.scene.clone(),
        engine: String::new(),
        animation: cfg.animation,
        still_frame: cfg.frame_start,
        frame_start: cfg.frame_start,
        frame_end: cfg.frame_end,
        frame_step: cfg.frame_step,
        path_prefix: cfg.path_prefix.clone(),
        path_pad: cfg.path_pad,
        path_suffix: cfg.path_suffix.clone(),
        template_ok: cfg.template_ok,
        is_movie: cfg.is_movie,
        overrides: cfg.overrides.clone(),
        resume: cfg.resume,
        auto_restart: cfg.auto_restart,
        max_retries: cfg.max_retries,
        verify: cfg.verify,
        ..Default::default()
    };
    job.recount();

    let data = store::AppData {
        jobs: vec![job.clone()],
        settings: store::Settings {
            clean_blender: cfg.clean,
            ..Default::default()
        },
        running: Default::default(),
    };
    let shared: store::Shared = Arc::new(Mutex::new(data));

    println!(
        "SELFTEST start: {} 帧 {}-{} -> {}",
        cfg.blend, cfg.frame_start, cfg.frame_end, cfg.path_prefix
    );
    runner::start(None, shared.clone(), job.id.clone());

    let deadline = Instant::now() + Duration::from_secs(cfg.timeout_sec.max(30));
    let mut last_line = String::new();
    let mut final_snap = job.clone();

    loop {
        std::thread::sleep(Duration::from_millis(400));
        let snap = {
            let Ok(s) = shared.lock() else { break };
            match s.jobs.first() {
                Some(j) => j.clone(),
                None => break,
            }
        };
        let line = format!(
            "{:?} {}/{} 帧 current={} 重启={}",
            snap.status, snap.progress.done, snap.progress.total,
            snap.progress.current, snap.progress.restarts
        );
        if line != last_line {
            println!("SELFTEST {}", line);
            last_line = line;
        }
        final_snap = snap.clone();
        if !matches!(
            snap.status,
            model::JobStatus::Running | model::JobStatus::Queued
        ) {
            break;
        }
        if Instant::now() > deadline {
            println!("SELFTEST timeout");
            break;
        }
    }

    let ok = final_snap.status == model::JobStatus::Done;
    let result = serde_json::json!({
        "status": format!("{:?}", final_snap.status),
        "done": final_snap.progress.done,
        "total": final_snap.progress.total,
        "failed": final_snap.progress.failed,
        "failedFrames": final_snap.failed_frames,
        "restarts": final_snap.progress.restarts,
        "elapsed": final_snap.elapsed,
        "message": final_snap.progress.message,
    });
    println!("SELFTEST RESULT {}", result);
    if !cfg.result_path.is_empty() {
        let _ = store::write_json(std::path::Path::new(&cfg.result_path), &result);
    }
    if ok {
        0
    } else if final_snap.progress.done > 0 {
        3
    } else {
        4
    }
}

/// 命令行：统计一批图片的元信息读取耗时（用于评估 image crate 的成本）
fn image_bench(dir: &str) {
    let path = std::path::Path::new(dir);
    let mut files: Vec<std::path::PathBuf> = if path.is_dir() {
        std::fs::read_dir(path)
            .map(|rd| rd.flatten().map(|e| e.path()).filter(|p| p.is_file()).collect())
            .unwrap_or_default()
    } else {
        vec![path.to_path_buf()]
    };
    files.sort();
    let total = files.len();
    let t = std::time::Instant::now();
    let mut ok = 0usize;
    let mut sample = String::new();
    for p in &files {
        if let Some(info) = media::read_image_info(p) {
            ok += 1;
            if sample.is_empty() {
                sample = format!(
                    "{}: {}x{} {}bit {}ch {}",
                    p.file_name().unwrap_or_default().to_string_lossy(),
                    info.width, info.height, info.depth, info.channels, info.format
                );
            }
        }
    }
    let dt = t.elapsed();
    println!(
        "图片 {} 个，识别 {} 个，总耗时 {:.1} ms，平均 {:.3} ms/个",
        total,
        ok,
        dt.as_secs_f64() * 1000.0,
        dt.as_secs_f64() * 1000.0 / total.max(1) as f64
    );
    if !sample.is_empty() {
        println!("样例 {}", sample);
    }
}
