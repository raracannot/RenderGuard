//! 路径、设置、原子化持久化，以及内嵌 Python 脚本的落地。

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use crate::model::Job;

pub const PROBE_PY: &str = include_str!("py/probe.py");
pub const BOOT_PY: &str = include_str!("py/boot.py");

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    /// 默认 Blender 可执行文件；新任务默认用它，任务里仍可单独指定
    pub default_blender: String,
    /// 用过的 Blender 路径（下拉历史）
    pub blender_paths: Vec<String>,
    pub last_blender: String,
    pub last_blend_dir: String,
    /// 卡死判定：多久没有任何输出/CPU 增长就认为挂了（秒）
    pub idle_timeout_sec: u64,
    /// 队列自动流转：一个完成任务后自动开始下一个
    pub queue_auto: bool,
    pub default_max_retries: u32,
    pub default_resume: bool,
    /// 渲染时用 --factory-startup，不加载任何插件（更稳、更干净）
    pub clean_blender: bool,
    /// 整个队列结束后自动关机
    pub shutdown_when_done: bool,
    /// 任务完成时提醒（本地横幅 + 窗口置前，不联网）
    pub notify_done: bool,
    /// 任务失败时提醒
    pub notify_fail: bool,
    /// 同时最多跑几个任务
    pub max_concurrent: u32,
    /// 界面皮肤编号（01/05/08/09/10/24/27/32/33/34/45/46/50/58/69）
    pub theme: String,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            default_blender: String::new(),
            blender_paths: Vec::new(),
            last_blender: String::new(),
            last_blend_dir: String::new(),
            idle_timeout_sec: 1800,
            queue_auto: true,
            default_max_retries: 3,
            default_resume: true,
            clean_blender: false,
            shutdown_when_done: false,
            notify_done: false,
            notify_fail: false,
            max_concurrent: 1,
            theme: "09".to_string(),
        }
    }
}

pub struct AppData {
    pub jobs: Vec<Job>,
    pub settings: Settings,
    pub running: HashMap<String, Arc<AtomicBool>>,
}

impl Default for AppData {
    fn default() -> Self {
        AppData {
            jobs: Vec::new(),
            settings: Settings::default(),
            running: HashMap::new(),
        }
    }
}

pub type Shared = Arc<std::sync::Mutex<AppData>>;

/// 便携模式：exe 同级存在 portable.txt 时，数据写到 exe 旁边。
/// 若 %LOCALAPPDATA% 不可写（沙箱 / 权限 / 杀软），自动回落到 exe 同级 data\。
pub fn data_dir() -> PathBuf {
    static CACHE: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    CACHE
        .get_or_init(|| {
            let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("."));
            let dir = exe
                .parent()
                .map(|p| p.to_path_buf())
                .unwrap_or_else(|| PathBuf::from("."));

            if dir.join("portable.txt").exists() || dir.join("RenderGuard.portable").exists() {
                let p = dir.join("data");
                let _ = fs::create_dir_all(&p);
                return p;
            }

            if let Ok(la) = std::env::var("LOCALAPPDATA") {
                let p = PathBuf::from(la).join("RenderGuard");
                if fs::create_dir_all(&p).is_ok() {
                    let probe = p.join(".write_test");
                    if fs::write(&probe, b"1").is_ok() {
                        let _ = fs::remove_file(&probe);
                        return p;
                    }
                }
            }

            let p = dir.join("data");
            let _ = fs::create_dir_all(&p);
            p
        })
        .clone()
}

pub fn ensure_dirs() -> std::io::Result<()> {
    let d = data_dir();
    for sub in ["", "logs", "jobs", "py", "state"] {
        let p = if sub.is_empty() { d.clone() } else { d.join(sub) };
        fs::create_dir_all(p)?;
    }
    Ok(())
}

/// 把内嵌脚本写到磁盘（Blender 只能通过 --python 执行真实文件）
pub fn ensure_scripts() -> std::io::Result<(PathBuf, PathBuf)> {
    let py = data_dir().join("py");
    fs::create_dir_all(&py)?;
    let probe = py.join("probe.py");
    let boot = py.join("boot.py");
    for (path, body) in [(&probe, PROBE_PY), (&boot, BOOT_PY)] {
        let need = match fs::read(path) {
            Ok(old) => old != body.as_bytes(),
            Err(_) => true,
        };
        if need {
            let tmp = path.with_extension("py.tmp");
            let mut f = fs::File::create(&tmp)?;
            f.write_all(body.as_bytes())?;
            f.sync_all().ok();
            drop(f);
            let _ = fs::remove_file(path);
            fs::rename(&tmp, path)?;
        }
    }
    Ok((probe, boot))
}

pub fn write_json<T: Serialize>(path: &Path, value: &T) -> std::io::Result<()> {
    let bytes = serde_json::to_vec_pretty(value)?;
    let tmp = path.with_extension(format!(
        "{}.tmp",
        path.extension().and_then(|e| e.to_str()).unwrap_or("json")
    ));
    {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(&bytes)?;
        f.sync_all().ok();
    }
    if path.exists() {
        let _ = fs::remove_file(path);
    }
    fs::rename(&tmp, path)
}

fn read_json<T: for<'de> Deserialize<'de> + Default>(path: &Path) -> T {
    fs::read(path)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

pub fn jobs_path() -> PathBuf {
    data_dir().join("state").join("jobs.json")
}
pub fn settings_path() -> PathBuf {
    data_dir().join("state").join("settings.json")
}

pub fn load_jobs() -> Vec<Job> {
    read_json::<Vec<Job>>(&jobs_path())
}

pub fn save_jobs(jobs: &[Job]) -> std::io::Result<()> {
    write_json(&jobs_path(), &jobs)
}

pub fn load_settings() -> Settings {
    read_json::<Settings>(&settings_path())
}

pub fn save_settings(s: &Settings) -> std::io::Result<()> {
    write_json(&settings_path(), s)
}

// ---------------------------------------------------------------- 维护与任务投递

/// 清理一天前的 Blender 会话临时目录（被强杀的进程不会自己删，久了会堆积）
pub fn cleanup_stale_temp() {
    let tmp = data_dir().join("tmp");
    let Ok(rd) = fs::read_dir(&tmp) else {
        return;
    };
    let now = SystemTime::now();
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        if !name.starts_with("blender_") {
            continue;
        }
        let old = e
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| now.duration_since(t).ok())
            .map(|age| age > Duration::from_secs(24 * 3600))
            .unwrap_or(false);
        if old {
            let _ = fs::remove_dir_all(e.path());
        }
    }
}

/// 外部投递入口：`renderguard.exe --enqueue job.json` 把文件丢进这里，
/// 运行中的程序会自动收编，避免直接改 jobs.json 造成覆盖。
pub fn inbox_dir() -> PathBuf {
    let p = data_dir().join("inbox");
    let _ = fs::create_dir_all(&p);
    p
}

/// 取走 inbox 里的任务文件（取到即删除）
pub fn drain_inbox() -> Vec<Job> {
    let dir = inbox_dir();
    let mut out = Vec::new();
    let Ok(rd) = fs::read_dir(&dir) else {
        return out;
    };
    for e in rd.flatten() {
        let p = e.path();
        let is_json = p
            .extension()
            .and_then(|x| x.to_str())
            .map(|x| x.eq_ignore_ascii_case("json"))
            .unwrap_or(false);
        if !is_json || !p.is_file() {
            continue;
        }
        if let Ok(text) = fs::read_to_string(&p) {
            if let Ok(j) = serde_json::from_str::<Job>(&text) {
                out.push(j);
            }
        }
        let _ = fs::remove_file(&p);
    }
    out
}
