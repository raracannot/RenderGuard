//! 把图片序列打包成视频：调用 Blender 内置的 FFmpeg 编码器，不依赖外部 ffmpeg。

use std::io::{BufRead, BufReader};
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};

use serde::{Deserialize, Serialize};
use tauri::Emitter;

use crate::store;

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const IMAGE_EXT: [&str; 8] = ["png", "jpg", "jpeg", "tif", "tiff", "bmp", "webp", "tga"];

/// 用户点「取消」时置位，run 的读行循环会杀掉 Blender
pub static CANCEL: AtomicBool = AtomicBool::new(false);

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase", default)]
pub struct PackCfg {
    pub src_dir: String,
    pub out_file: String,
    pub fps: u32,
    /// name_asc / name_desc / time_asc / time_desc
    pub order: String,
    /// MPEG4 / MKV / QUICKTIME / WEBM
    pub format: String,
    /// H264 / H265 / AV1 / WEBM / PRORES
    pub codec: String,
    /// LOSSLESS / PERC_LOSSLESS / HIGH / MEDIUM / LOW / VERYLOW / LOWEST
    pub quality: String,
    /// 可选的音轨文件（音频或带音轨的视频）
    pub audio: String,
    pub bg: String,
    /// 保留 alpha（只有 MOV + PNG / QTRLE / ProRes 4444 支持）
    pub rgba: bool,
    /// ProRes 档位：4444 / 4444_XQ / 422_HQ / 422_STD / 422_LT / 422_PROXY
    pub prores: String,
}

impl Default for PackCfg {
    fn default() -> Self {
        PackCfg {
            src_dir: String::new(),
            out_file: String::new(),
            fps: 30,
            order: "name_asc".into(),
            format: "MPEG4".into(),
            codec: "H264".into(),
            quality: "MEDIUM".into(),
            audio: String::new(),
            bg: "#0a0a0a".into(),
            rgba: false,
            prores: "4444".into(),
        }
    }
}

#[derive(Clone, Serialize, Default, Debug)]
#[serde(rename_all = "camelCase")]
pub struct PackState {
    pub running: bool,
    pub percent: f64,
    pub frame: i64,
    pub total: i64,
    pub message: String,
    pub out_file: String,
    pub ok: bool,
}

/// 数一数文件夹里有多少张图，用来算进度
pub fn count_frames(dir: &str) -> i64 {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return 0;
    };
    rd.flatten()
        .filter(|e| {
            let p = e.path();
            p.is_file()
                && p.extension()
                    .and_then(|x| x.to_str())
                    .map(|x| IMAGE_EXT.contains(&x.to_ascii_lowercase().as_str()))
                    .unwrap_or(false)
        })
        .count() as i64
}

fn emit_log(app: &Option<tauri::AppHandle>, level: &str, text: &str) {
    if let Some(a) = app {
        let _ = a.emit(
            "rg://log",
            serde_json::json!({
                "id": "pack",
                "lines": [{ "level": level, "text": text }],
            }),
        );
    }
}

fn emit_state(app: &Option<tauri::AppHandle>, st: &PackState) {
    if let Some(a) = app {
        let _ = a.emit("rg://pack", st);
    }
}

/// 抽一行 Blender 输出里的帧号（`Fra:12` 或 `Fra:12 Mem:...`）
fn parse_frame(line: &str) -> Option<i64> {
    let idx = line.find("Fra:")?;
    let rest = line[idx + 4..].trim_start();
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    digits.parse().ok()
}

pub fn run(app: Option<tauri::AppHandle>, blender: &str, cfg: &PackCfg) -> Result<String, String> {
    if cfg.src_dir.trim().is_empty() {
        return Err("请先选择序列文件夹".into());
    }
    if !Path::new(&cfg.src_dir).is_dir() {
        return Err(format!("序列文件夹不存在：{}", cfg.src_dir));
    }
    if blender.trim().is_empty() || !Path::new(blender).exists() {
        return Err(format!("Blender 路径不可用：{}", blender));
    }

    let total = count_frames(&cfg.src_dir);
    if total == 0 {
        return Err("这个文件夹里没有可用的图片（png / jpg / tif / bmp / webp / tga）".into());
    }

    // 输出路径：没填就放在序列文件夹旁边，默认用文件夹名
    let mut out = cfg.out_file.trim().to_string();
    if out.is_empty() {
        let stem = Path::new(&cfg.src_dir)
            .file_name()
            .and_then(|x| x.to_str())
            .unwrap_or("output");
        out = Path::new(&cfg.src_dir)
            .parent()
            .map(|p| p.join(format!("{}.mp4", stem)))
            .unwrap_or_else(|| PathBuf::from(format!("{}.mp4", stem)))
            .to_string_lossy()
            .to_string();
    }
    if let Some(dir) = Path::new(&out).parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("无法创建输出目录：{}", e))?;
    }

    // 配置写盘交给 pack.py 读
    let dir = store::data_dir().join("pack");
    std::fs::create_dir_all(&dir).ok();
    let cfg_path = dir.join(format!(
        "cfg-{}.json",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0)
    ));
    let mut effective = cfg.clone();
    effective.out_file = out.clone();
    store::write_json(&cfg_path, &effective).map_err(|e| format!("写入打包参数失败：{}", e))?;

    let script = store::pack_script();
    if !script.exists() {
        store::ensure_scripts().map_err(|e| format!("释放打包脚本失败：{}", e))?;
    }

    emit_log(
        &app,
        "ok",
        &format!(
            "开始打包：{} 张图 → {}（{} fps，{} {}）",
            total, out, cfg.fps, cfg.format, cfg.codec
        ),
    );

    let mut cmd = Command::new(blender);
    cmd.arg("-b")
        .arg("--factory-startup")
        .arg("--python")
        .arg(&script)
        .arg("--")
        .arg(&cfg_path)
        .current_dir(store::data_dir().join("cwd"))
        .env("TEMP", store::data_dir().join("tmp"))
        .env("TMP", store::data_dir().join("tmp"))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .creation_flags(CREATE_NO_WINDOW);

    let _ = std::fs::create_dir_all(store::data_dir().join("cwd"));
    let _ = std::fs::create_dir_all(store::data_dir().join("tmp"));

    let mut child = cmd.spawn().map_err(|e| format!("启动 Blender 失败：{}", e))?;
    let mut state = PackState {
        running: true,
        total,
        out_file: out.clone(),
        message: "编码中…".into(),
        ..Default::default()
    };
    emit_state(&app, &state);

    let stderr = child.stderr.take();
    let mut err_lines: Vec<String> = Vec::new();
    let mut ok = false;
    let mut fail_msg: Option<String> = None;

    if let Some(out_pipe) = child.stdout.take() {
        let reader = BufReader::new(out_pipe);
        let mut last_emit = std::time::Instant::now();
        for line in reader.lines().map_while(Result::ok) {
            if CANCEL.load(Ordering::Relaxed) {
                let _ = child.kill();
                fail_msg = Some("已取消".into());
                break;
            }
            if let Some(f) = parse_frame(&line) {
                state.frame = f;
                state.percent = if state.total > 0 {
                    (f as f64 / state.total as f64 * 100.0).min(100.0)
                } else {
                    0.0
                };
            }
            if line.starts_with("PACK_FAIL") {
                fail_msg = Some(line.trim_start_matches("PACK_FAIL").trim().to_string());
            } else if line.starts_with("PACK_OK") {
                ok = true;
            } else if line.starts_with("PACK_WARN") {
                emit_log(&app, "warn", line.trim_start_matches("PACK_WARN").trim());
            } else if line.starts_with("PACK_INFO") {
                emit_log(&app, "info", line.trim());
            }
            if last_emit.elapsed() >= std::time::Duration::from_millis(250) {
                last_emit = std::time::Instant::now();
                emit_state(&app, &state);
            }
        }
    }

    // 收一下 stderr，失败时给用户看关键几行
    if let Some(se) = stderr {
        for line in BufReader::new(se).lines().map_while(Result::ok) {
            let t = line.trim();
            if t.is_empty() || t.contains("Unable to delete") {
                continue;
            }
            if t.contains("Error") || t.contains("error") || t.contains("Traceback") {
                err_lines.push(t.chars().take(200).collect());
            }
        }
    }

    let status = child.wait().map_err(|e| format!("等待 Blender 结束失败：{}", e))?;
    state.running = false;
    state.percent = if ok { 100.0 } else { state.percent };
    state.ok = ok;

    if !ok {
        state.message = fail_msg
            .clone()
            .or_else(|| err_lines.last().cloned())
            .unwrap_or_else(|| format!("Blender 退出码 {:?}", status.code()));
        emit_state(&app, &state);
        emit_log(&app, "error", &state.message);
        let _ = std::fs::remove_file(&cfg_path);
        return Err(state.message.clone());
    }

    state.message = format!("已生成：{}", out);
    emit_state(&app, &state);
    emit_log(&app, "ok", &state.message);
    let _ = std::fs::remove_file(&cfg_path);
    Ok(out)
}
