//! 预览 / 执行 / 撤销（两阶段改名，避免 A→B 与 B→C 互相覆盖）

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use super::*;

const ILLEGAL: [char; 9] = ['\\', '/', ':', '*', '?', '"', '<', '>', '|'];

fn sanitize(name: &str) -> (String, bool) {
    if name.chars().any(|c| ILLEGAL.contains(&c)) {
        (
            name.chars().filter(|c| !ILLEGAL.contains(c)).collect(),
            true,
        )
    } else {
        (name.to_string(), false)
    }
}

fn target_of(row: &Row) -> PathBuf {
    Path::new(&row.src)
        .parent()
        .map(|p| p.join(format!("{}{}", row.new_name, row.new_ext)))
        .unwrap_or_else(|| PathBuf::from(&row.src))
}

/// 生成重命名预览
pub fn preview(cfg: &RenameCfg) -> Preview {
    let mut rows = scan(cfg);
    if rows.is_empty() {
        return Preview {
            error: "没有找到文件（检查目录、包含/排除条件、扩展名过滤）".into(),
            ..Default::default()
        };
    }

    let has_same = cfg.template.to_uppercase().contains("<SAME");
    let mut same_now = cfg.start;
    let mut number_now = cfg.start;

    for row in rows.iter_mut() {
        let mut ctx = Ctx {
            row,
            number: number_now,
            padding: cfg.padding,
            zero_pad: cfg.zero_pad,
            same_value: same_now,
        };
        let raw = expand(&cfg.template, &mut ctx);
        let (clean, fixed) = sanitize(&raw);
        row.new_name = clean.trim().to_string();
        row.new_ext = row.ext.clone();
        if fixed {
            row.issue = "illegal".into();
            row.detail = "文件名里有非法字符，已自动去掉".into();
        }
        if row.new_name.is_empty() && row.issue.is_empty() {
            row.issue = "illegal".into();
            row.detail = "新文件名为空".into();
        }
        number_now += cfg.increment;
        if has_same {
            same_now += cfg.increment;
        }
    }

    // 冲突检测：批内重名 + 目标已存在
    let mut seen: HashMap<String, usize> = HashMap::new();
    for i in 0..rows.len() {
        let target = target_of(&rows[i]).to_string_lossy().to_ascii_lowercase();
        if rows[i].issue.is_empty() {
            if let Some(prev) = seen.get(&target) {
                rows[i].issue = "dup".into();
                rows[i].detail = format!("与第 {} 条目标重名", prev + 1);
            } else {
                let src_key = rows[i].src.to_ascii_lowercase();
                let exists = Path::new(&target).exists();
                if exists && target != src_key {
                    rows[i].issue = "exists".into();
                    rows[i].detail = "目标文件已存在".into();
                }
            }
        }
        seen.insert(target, i);

        let full = format!("{}{}", rows[i].new_name, rows[i].new_ext);
        if full.chars().count() > 200 && rows[i].issue.is_empty() {
            rows[i].issue = "long".into();
            rows[i].detail = "文件名过长".into();
        }
    }

    let bad = rows.iter().filter(|r| !r.issue.is_empty()).count();
    let total = rows.len();
    Preview {
        rows,
        total,
        bad,
        tags: tags_in(&cfg.template),
        error: String::new(),
    }
}

/// 撤销记录：记录「最终名 → 原路径」，撤销时倒着改回去
#[derive(serde::Serialize, serde::Deserialize, Clone)]
struct UndoRecord {
    time: i64,
    pairs: Vec<(String, String)>,
}

fn undo_dir() -> PathBuf {
    crate::store::data_dir().join("rename")
}

fn list_undo() -> Vec<(i64, PathBuf)> {
    let mut files: Vec<(i64, PathBuf)> = Vec::new();
    if let Ok(rd) = std::fs::read_dir(undo_dir()) {
        for e in rd.flatten() {
            let p = e.path();
            if let Some(stem) = p.file_stem().and_then(|s| s.to_str()) {
                if let Some(num) = stem.strip_prefix("undo-") {
                    if let Ok(v) = num.parse::<i64>() {
                        files.push((v, p));
                    }
                }
            }
        }
    }
    files.sort();
    files
}

fn write_undo(record: &UndoRecord) -> std::io::Result<()> {
    std::fs::create_dir_all(undo_dir())?;
    let mut files = list_undo();
    while files.len() >= 20 {
        let (_, old) = files.remove(0);
        let _ = std::fs::remove_file(old);
    }
    let next = list_undo().last().map(|(v, _)| *v + 1).unwrap_or(1);
    crate::store::write_json(&undo_dir().join(format!("undo-{}.json", next)), record)
}

/// 执行重命名：阶段一把源文件改成临时名，阶段二改成目标名
pub fn apply(cfg: &RenameCfg) -> Result<(usize, usize, usize), String> {
    let pv = preview(cfg);
    if !pv.error.is_empty() {
        return Err(pv.error);
    }

    let mode = cfg.conflict.as_str();
    let mut done: Vec<(String, String)> = Vec::new(); // (原路径, 最终路径)
    let mut skipped = 0usize;
    let mut staged: Vec<(PathBuf, PathBuf, PathBuf)> = Vec::new(); // (原, 临时, 目标)

    for (i, row) in pv.rows.iter().enumerate() {
        let src = PathBuf::from(&row.src);
        let dst = target_of(row);
        if row.new_name.is_empty() || src == dst {
            continue;
        }
        match row.issue.as_str() {
            "dup" | "exists" if mode != "overwrite" && mode != "auto" => {
                skipped += 1;
                continue;
            }
            "long" => {
                skipped += 1;
                continue;
            }
            _ => {}
        }
        let tmp = src
            .parent()
            .map(|p| p.join(format!("__rg_tmp_{}_{}", i, fnv(&row.new_name))))
            .unwrap_or_else(|| src.with_extension("__rg_tmp"));
        match std::fs::rename(&src, &tmp) {
            Ok(_) => staged.push((src, tmp, dst)),
            Err(_) => skipped += 1,
        }
    }

    for (src, tmp, dst) in staged {
        let mut final_dst = dst.clone();
        if final_dst.exists() {
            match mode {
                "overwrite" => {
                    let _ = std::fs::remove_file(&final_dst);
                }
                "auto" => {
                    let stem = final_dst
                        .file_stem()
                        .map(|s| s.to_string_lossy().to_string())
                        .unwrap_or_default();
                    let ext = final_dst
                        .extension()
                        .map(|s| format!(".{}", s.to_string_lossy()))
                        .unwrap_or_default();
                    let dir = final_dst.parent().map(|p| p.to_path_buf()).unwrap_or_default();
                    let mut n = 1;
                    while final_dst.exists() {
                        final_dst = dir.join(format!("{}_{}{}", stem, n, ext));
                        n += 1;
                    }
                }
                _ => {}
            }
        }
        match std::fs::rename(&tmp, &final_dst) {
            Ok(_) => done.push((
                src.to_string_lossy().to_string(),
                final_dst.to_string_lossy().to_string(),
            )),
            Err(_) => {
                let _ = std::fs::rename(&tmp, &src);
                skipped += 1;
            }
        }
    }

    if !done.is_empty() {
        let _ = write_undo(&UndoRecord {
            time: now_secs(),
            pairs: done.clone(),
        });
    }
    Ok((done.len(), skipped, pv.total))
}

/// 撤销最近一次：把「最终名」改回「原路径」
pub fn undo_last() -> Result<usize, String> {
    let files = list_undo();
    let Some((_, path)) = files.last() else {
        return Err("没有可撤销的操作".into());
    };
    let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    let rec: UndoRecord = serde_json::from_str(&text).map_err(|e| e.to_string())?;

    let mut n = 0usize;
    for (orig, final_path) in rec.pairs.iter().rev() {
        if !Path::new(final_path).exists() {
            continue;
        }
        if Path::new(orig).exists() {
            continue;
        }
        if std::fs::rename(final_path, orig).is_ok() {
            n += 1;
        }
    }
    let _ = std::fs::remove_file(path);
    Ok(n)
}

/// 上一次操作是否可以撤销
pub fn undo_available() -> Option<i64> {
    list_undo().last().map(|(_, p)| {
        p.file_stem()
            .and_then(|s| s.to_str())
            .and_then(|s| s.strip_prefix("undo-"))
            .and_then(|s| s.parse().ok())
            .unwrap_or(0)
    })
}

fn fnv(s: &str) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in s.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}
