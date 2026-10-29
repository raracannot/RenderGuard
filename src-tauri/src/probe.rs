//! 调用 Blender 读取 .blend 的真实渲染参数。

use std::os::windows::process::CommandExt;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::model::ProbeResult;
use crate::store;
use crate::winproc;

fn tail_of(path: &std::path::Path, lines: usize) -> String {
    let Ok(text) = std::fs::read_to_string(path) else {
        return String::new();
    };
    let all: Vec<&str> = text.lines().collect();
    let start = all.len().saturating_sub(lines);
    all[start..].join("\n")
}

pub fn probe(blender: &str, blend: &str) -> Result<ProbeResult, String> {
    if !std::path::Path::new(blender).is_file() {
        return Err(format!("找不到 Blender：{}", blender));
    }
    if !std::path::Path::new(blend).is_file() {
        return Err(format!("找不到文件：{}", blend));
    }

    let dir = store::data_dir();
    let (probe_py, _) = store::ensure_scripts().map_err(|e| {
        format!(
            "无法写入数据目录 {}：{}",
            dir.display(),
            e
        )
    })?;
    let out = dir
        .join("state")
        .join(format!("probe-{}.json", std::process::id()));
    let _ = std::fs::remove_file(&out);

    let log = dir.join("logs").join("probe.log");
    let _ = std::fs::create_dir_all(log.parent().unwrap_or(&dir));
    let logf = std::fs::File::create(&log).ok();

    let mut cmd = Command::new(blender);
    cmd.arg("-b")
        .arg(blend)
        .arg("--python-exit-code")
        .arg("1")
        .arg("--python")
        .arg(&probe_py)
        .arg("-noaudio")
        .env("RG_PROBE_OUT", &out)
        .creation_flags(winproc::no_window());

    match logf {
        Some(f) => {
            if let (Ok(a), Ok(b)) = (f.try_clone(), f.try_clone()) {
                cmd.stdout(Stdio::from(a)).stderr(Stdio::from(b));
            }
        }
        None => {
            cmd.stdout(Stdio::null()).stderr(Stdio::null());
        }
    }

    let mut child = cmd
        .spawn()
        .map_err(|e| format!("无法启动 Blender：{}", e))?;
    winproc::attach_to_job(child.id());

    let deadline = Instant::now() + Duration::from_secs(600);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                if !status.success() && !out.exists() {
                    return Err(format!(
                        "Blender 读取场景失败（{}）。\n{}",
                        winproc::exit_text(status.code().unwrap_or(-1)),
                        tail_of(&log, 25)
                    ));
                }
                break;
            }
            Ok(None) => {
                if Instant::now() > deadline {
                    winproc::kill_tree(child.id());
                    let _ = child.wait();
                    return Err("读取场景超时（10 分钟）".into());
                }
                std::thread::sleep(Duration::from_millis(150));
            }
            Err(e) => return Err(e.to_string()),
        }
    }

    let bytes = std::fs::read(&out).map_err(|_| {
        format!(
            "未取得场景信息。\n{}",
            tail_of(&log, 25)
        )
    })?;
    let _ = std::fs::remove_file(&out);
    let res: ProbeResult = serde_json::from_slice(&bytes)
        .map_err(|e| format!("场景信息解析失败：{}", e))?;
    if !res.ok {
        return Err(format!("场景读取出错：{}", res.error));
    }
    Ok(res)
}
