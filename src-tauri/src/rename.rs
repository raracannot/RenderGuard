//! 批量重命名引擎：替换符 `<TAG/OP:ARG>` + 两阶段改名 + 撤销。
//!
//! 替换符与操作符的语义尽量与 Blender 插件 AA-RENAME 保持一致，
//! 但实现上完全独立（不依赖 Blender 进程）。

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use md5::Md5;
use sha1::Sha1;
use sha2::{Digest, Sha256};

use crate::media;
use crate::videoinfo;

mod plan;

pub use plan::{apply, preview, undo_available, undo_last};

// ---------------------------------------------------------------- 配置与数据

#[derive(serde::Serialize, serde::Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase", default)]
pub struct RenameCfg {
    /// 一个或多个目录，逗号分隔
    pub dirs: String,
    pub depth: u32,
    /// 文件名需包含的字符，空则不限定
    pub include: String,
    /// 文件名需排除的字符
    pub exclude: String,
    /// 只处理这些扩展名（不含点），空则全部
    pub exts: String,
    /// name_asc / name_desc / time_asc / time_desc / size_asc / size_desc
    pub sort: String,
    pub template: String,
    pub start: i64,
    pub increment: i64,
    pub padding: usize,
    pub zero_pad: bool,
    pub randomize: bool,
    /// skip / overwrite / auto
    pub conflict: String,
}

impl Default for RenameCfg {
    fn default() -> Self {
        RenameCfg {
            dirs: String::new(),
            depth: 0,
            include: String::new(),
            exclude: String::new(),
            exts: String::new(),
            sort: "name_asc".into(),
            template: "<NUMBER>_<SELF>".into(),
            start: 1,
            increment: 1,
            padding: 4,
            zero_pad: true,
            randomize: false,
            conflict: "skip".into(),
        }
    }
}

#[derive(serde::Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Row {
    pub src: String,
    pub name: String,
    pub ext: String,
    pub size: u64,
    pub mtime: i64,
    pub new_name: String,
    pub new_ext: String,
    /// ok / dup / exists / illegal / long
    pub issue: String,
    pub detail: String,
}

#[derive(serde::Serialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct Preview {
    pub rows: Vec<Row>,
    pub total: usize,
    pub bad: usize,
    /// 模板里用到的替换符，便于前端提示
    pub tags: Vec<String>,
    pub error: String,
}

// ---------------------------------------------------------------- 扫描

struct FileEntry {
    path: PathBuf,
    name: String,
    ext: String,
    size: u64,
    mtime: i64,
}

fn mtime_of(md: &std::fs::Metadata) -> i64 {
    md.modified()
        .ok()
        .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn natural_key(name: &str) -> Vec<(u8, String, i64)> {
    // 把字符串切成 数字段/非数字段，数字按数值比较
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut digits = false;
    for ch in name.to_lowercase().chars() {
        let d = ch.is_ascii_digit();
        if d != digits && !cur.is_empty() {
            out.push(if digits {
                (1, String::new(), cur.parse::<i64>().unwrap_or(0))
            } else {
                (0, cur.clone(), 0)
            });
            cur.clear();
        }
        digits = d;
        cur.push(ch);
    }
    if !cur.is_empty() {
        out.push(if digits {
            (1, String::new(), cur.parse::<i64>().unwrap_or(0))
        } else {
            (0, cur, 0)
        });
    }
    out
}

fn walk(dir: &Path, depth: u32, max_depth: u32, out: &mut Vec<FileEntry>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        let Ok(md) = e.metadata() else { continue };
        if md.is_dir() {
            if depth < max_depth {
                walk(&p, depth + 1, max_depth, out);
            }
            continue;
        }
        let name = p
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
        let ext = p
            .extension()
            .map(|s| format!(".{}", s.to_string_lossy()))
            .unwrap_or_default();
        out.push(FileEntry {
            path: p,
            name,
            ext,
            size: md.len(),
            mtime: mtime_of(&md),
        });
    }
}

/// 扫描目录并按规则排序
pub fn scan(cfg: &RenameCfg) -> Vec<Row> {
    let mut entries: Vec<FileEntry> = Vec::new();
    for d in cfg.dirs.split(',').map(|s| s.trim()).filter(|s| !s.is_empty()) {
        walk(Path::new(d), 0, cfg.depth, &mut entries);
    }

    let exts: Vec<String> = cfg
        .exts
        .split(',')
        .map(|s| s.trim().trim_start_matches('.').to_ascii_lowercase())
        .filter(|s| !s.is_empty())
        .collect();
    entries.retain(|e| {
        let ext_ok = exts.is_empty()
            || exts.contains(&e.ext.trim_start_matches('.').to_ascii_lowercase());
        let inc_ok = cfg.include.is_empty() || e.name.contains(&cfg.include);
        let exc_ok = cfg.exclude.is_empty() || !e.name.contains(&cfg.exclude);
        ext_ok && inc_ok && exc_ok
    });

    match cfg.sort.as_str() {
        "name_desc" => entries.sort_by(|a, b| natural_key(&b.name).cmp(&natural_key(&a.name))),
        "time_asc" => entries.sort_by_key(|e| (e.mtime, e.path.clone())),
        "time_desc" => {
            entries.sort_by(|a, b| (b.mtime, b.path.clone()).cmp(&(a.mtime, a.path.clone())))
        }
        "size_asc" => entries.sort_by_key(|e| (e.size, e.path.clone())),
        "size_desc" => entries.sort_by(|a, b| (b.size, b.path.clone()).cmp(&(a.size, a.path.clone()))),
        _ => entries.sort_by(|a, b| natural_key(&a.name).cmp(&natural_key(&b.name))),
    }

    entries
        .into_iter()
        .map(|e| Row {
            src: e.path.to_string_lossy().to_string(),
            name: e.name,
            ext: e.ext,
            size: e.size,
            mtime: e.mtime,
            new_name: String::new(),
            new_ext: String::new(),
            issue: String::new(),
            detail: String::new(),
        })
        .collect()
}

// ---------------------------------------------------------------- 模板展开

pub fn tags_in(template: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = template;
    while let Some(s) = rest.find('<') {
        let after = &rest[s + 1..];
        let Some(e) = after.find('>') else { break };
        let tag = &after[..e];
        let base = tag.split('/').next().unwrap_or("").to_uppercase();
        if !base.is_empty() && !out.contains(&base) {
            out.push(base);
        }
        rest = &after[e + 1..];
    }
    out
}

struct Ctx<'a> {
    row: &'a Row,
    number: i64,
    padding: usize,
    zero_pad: bool,
    /// <SAME> 的值：整批共用一个递增计数
    same_value: i64,
}

fn pad_num(v: i64, width: usize) -> String {
    if width == 0 {
        return v.to_string();
    }
    let neg = v < 0;
    let s = v.abs().to_string();
    let mut out = if s.len() >= width {
        s
    } else {
        format!("{}{}", "0".repeat(width - s.len()), s)
    };
    if neg {
        out.insert(0, '-');
    }
    out
}

fn is_cn(c: char) -> bool {
    matches!(c as u32,
        0x4E00..=0x9FFF | 0x3400..=0x4DBF | 0xF900..=0xFAFF | 0x3000..=0x303F)
}

fn expand_tag(ctx: &mut Ctx, tag: &str) -> String {
    let mut parts = tag.splitn(2, '/');
    let base = parts.next().unwrap_or("").trim().to_uppercase();
    let opstr = parts.next().unwrap_or("");


    let text = match base.as_str() {
        "SELF" => ctx.row.name.clone(),
        "EXT" => ctx.row.ext.clone(),
        "NUMBER" => pad_num(ctx.number, if ctx.zero_pad { ctx.padding } else { 0 }),
        "NUMBER_CN" => num_to_cn(ctx.number),
        "NUMBER_EN" => num_to_en(ctx.number),
        "NUMBER_RM" => num_to_rm(ctx.number),
        "FOLDER" => Path::new(&ctx.row.src)
            .parent()
            .and_then(|p| p.file_name())
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default(),
        "B" => ctx.row.size.to_string(),
        "KB" => format!("{}", ctx.row.size / 1024),
        "MB" => format!("{}", ctx.row.size / 1024 / 1024),
        "GB" => format!("{:.1}", ctx.row.size as f64 / 1024.0 / 1024.0 / 1024.0),
        "CN" => ctx.row.name.chars().filter(|c| is_cn(*c)).collect(),
        "EN" => ctx
            .row
            .name
            .chars()
            .filter(|c| c.is_ascii_alphabetic())
            .collect(),
        "NU" => ctx
            .row
            .name
            .chars()
            .filter(|c| c.is_ascii_digit())
            .collect(),
        "NUMBER_RAW" => ctx.number.to_string(),
        // 当前时间
        "DATE" | "YYYY" | "MM" | "DD" | "HH" | "H24" | "H12" | "MN" | "SS" | "MS" => {
            date_part(&base, now_secs())
        }
        // 修改时间
        "MDATE" | "MYYYY" | "MMM" | "MDD" | "MHH" | "MH12" | "MMN" | "MSS" => {
            date_part(&base[1..], ctx.row.mtime)
        }
        // 创建时间（Win32 GetFileTime，取不到时退回修改时间）
        "CDATE" | "CYYYY" | "CMM" | "CDD" | "CHH" | "CH12" | "CMN" | "CSS" => {
            let created = crate::winproc::creation_time(std::path::Path::new(&ctx.row.src))
                .unwrap_or(ctx.row.mtime);
            date_part(&base[1..], created)
        }
        // 媒体信息
        "PX" | "PXX" | "PXY" | "BIT" => {
            if let Some(i) = media::read_image_info(Path::new(&ctx.row.src)) {
                match base.as_str() {
                    "PX" => format!("{}x{}", i.width, i.height),
                    "PXX" => i.width.to_string(),
                    "PXY" => i.height.to_string(),
                    _ => i.depth.to_string(),
                }
            } else if let Some(v) = videoinfo::probe_auto(&ctx.row.src) {
                match base.as_str() {
                    "PX" => format!("{}x{}", v.width, v.height),
                    "PXX" => v.width.to_string(),
                    "PXY" => v.height.to_string(),
                    _ => String::new(),
                }
            } else {
                String::new()
            }
        }
        "DURATION" => videoinfo::probe_auto(&ctx.row.src)
            .map(|v| format!("{:.1}", v.duration))
            .unwrap_or_default(),
        "FPS" => videoinfo::probe_auto(&ctx.row.src)
            .map(|v| {
                if (v.fps - v.fps.round()).abs() < 0.001 {
                    format!("{}", v.fps.round() as i64)
                } else {
                    format!("{:.3}", v.fps).trim_end_matches('0').to_string()
                }
            })
            .unwrap_or_default(),
        "MD5" | "SHA1" | "SHA256" => file_hash(&ctx.row.src, &base),
        "SAME" => pad_num(ctx.same_value, if ctx.zero_pad { ctx.padding } else { 0 }),
        _ => return format!("<{}>", tag),
    };

    // 依次应用操作符
    let mut out = text;
    for op in opstr.split('/').filter(|s| !s.is_empty()) {
        out = text_operation(&out, op);
    }
    out
}

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// 极简日期拆分（不引 chrono，用本地时间近似 UTC+8 由系统提供）
fn date_part(tag: &str, secs: i64) -> String {
    let (y, mo, d, h, mi, s) = civil_from_unix(secs);
    match tag {
        "DATE" | "MDATE" => format!("{:04}{:02}{:02}", y, mo, d),
        "YYYY" | "MYYYY" => format!("{:04}", y),
        "MM" => format!("{:02}", mo),
        "MMM" => format!("{:02}", mo),
        "DD" | "MDD" => format!("{:02}", d),
        "HH" | "H24" | "MHH" => format!("{:02}", h),
        "H12" | "MH12" => {
            let h12 = if h % 12 == 0 { 12 } else { h % 12 };
            format!("{:02}", h12)
        }
        "MN" | "MMN" => format!("{:02}", mi),
        "SS" | "MSS" => format!("{:02}", s),
        "MS" => "000".to_string(),
        _ => String::new(),
    }
}

fn civil_from_unix(secs: i64) -> (i64, u32, u32, u32, u32, u32) {
    let days = secs.div_euclid(86400);
    let rem = secs.rem_euclid(86400);
    let (h, mi, s) = (
        (rem / 3600) as u32,
        ((rem % 3600) / 60) as u32,
        (rem % 60) as u32,
    );
    // Howard Hinnant 的 civil_from_days
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d, h, mi, s)
}

fn file_hash(path: &str, algo: &str) -> String {
    let Ok(bytes) = std::fs::read(path) else {
        return String::new();
    };
    match algo {
        "MD5" => format!("{:x}", Md5::digest(&bytes)),
        "SHA1" => format!("{:x}", Sha1::digest(&bytes)),
        _ => format!("{:x}", Sha256::digest(&bytes)),
    }
}

pub fn expand(template: &str, ctx: &mut Ctx) -> String {
    let mut out = String::new();
    let mut rest = template;
    while let Some(s) = rest.find('<') {
        out.push_str(&rest[..s]);
        let after = &rest[s + 1..];
        let Some(e) = after.find('>') else {
            out.push_str(&rest[s..]);
            return out;
        };
        let tag = &after[..e];
        out.push_str(&expand_tag(ctx, tag));
        rest = &after[e + 1..];
    }
    out.push_str(rest);
    out
}

// ---------------------------------------------------------------- 操作符

fn split_args(rest: &str) -> Vec<&str> {
    rest.split(':').collect()
}

pub fn text_operation(text: &str, op: &str) -> String {
    let (name, rest) = match op.find(':') {
        Some(i) => (&op[..i], &op[i + 1..]),
        None => (op, ""),
    };
    let name = name.to_uppercase();
    let args = split_args(rest);
    let arg = |i: usize| -> String { args.get(i).map(|s| s.to_string()).unwrap_or_default() };

    let chars: Vec<char> = text.chars().collect();
    let clen = chars.len();

    match name.as_str() {
        "UPPER" => text.to_uppercase(),
        "LOWER" => text.to_lowercase(),
        "TITLE" => text
            .split(' ')
            .map(|w| {
                let mut c = w.chars();
                match c.next() {
                    Some(f) => f.to_uppercase().collect::<String>() + &c.as_str().to_lowercase(),
                    None => String::new(),
                }
            })
            .collect::<Vec<_>>()
            .join(" "),
        "CAPITALIZE" => {
            let mut c = text.chars();
            match c.next() {
                Some(f) => f.to_uppercase().collect::<String>() + &c.as_str().to_lowercase(),
                None => String::new(),
            }
        }
        "SWAPCASE" => text
            .chars()
            .map(|c| {
                if c.is_uppercase() {
                    c.to_lowercase().to_string()
                } else if c.is_lowercase() {
                    c.to_uppercase().to_string()
                } else {
                    c.to_string()
                }
            })
            .collect(),
        "A_STRIP" => text.chars().filter(|c| !c.is_whitespace()).collect(),
        "STRIP" => text.trim().to_string(),
        "L_STRIP" => text.trim_start().to_string(),
        "R_STRIP" => text.trim_end().to_string(),
        "LEN" => clen.to_string(),
        "REVERSE" => chars.iter().rev().collect(),
        "REPEAT" => text.repeat(arg(0).parse::<usize>().unwrap_or(1)),
        "COUNT" => text.matches(arg(0).as_str()).count().to_string(),
        "SHUFFLE" => {
            let mut v = chars.clone();
            let mut seed = now_secs() as u64 ^ (clen as u64) << 16;
            for i in (1..v.len()).rev() {
                seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                let j = (seed >> 33) as usize % (i + 1);
                v.swap(i, j);
            }
            v.into_iter().collect()
        }
        "CN" => chars.iter().filter(|c| is_cn(**c)).collect(),
        "CN_DEL" => chars.iter().filter(|c| !is_cn(**c)).collect(),
        "EN" => chars.iter().filter(|c| c.is_ascii_alphabetic()).collect(),
        "EN_DEL" => chars.iter().filter(|c| !c.is_ascii_alphabetic()).collect(),
        "NU" => chars.iter().filter(|c| c.is_ascii_digit()).collect(),
        "NU_DEL" => chars.iter().filter(|c| !c.is_ascii_digit()).collect(),
        "CNE" => chars
            .iter()
            .filter(|c| is_cn(**c) || c.is_ascii_alphanumeric())
            .collect(),
        "CNE_DEL" => chars
            .iter()
            .filter(|c| !(is_cn(**c) || c.is_ascii_alphanumeric()))
            .collect(),
        "CUT" => cut_text(&chars, &arg(0)),
        "DEL" => del_text(&chars, &arg(0)),
        "PAD" => pad_text(text, &arg(0), &arg(1)),
        "SPLIT" => {
            let sep = arg(0);
            let idx: usize = arg(1).parse().unwrap_or(1);
            text.split(sep.as_str())
                .nth(idx.saturating_sub(1))
                .unwrap_or("")
                .to_string()
        }
        "ADD" => {
            let add = arg(0);
            let pos: usize = arg(1).parse().unwrap_or(0);
            if pos == 0 {
                format!("{}{}", add, text)
            } else {
                let mut s: String = chars.iter().take(pos).collect();
                s.push_str(&add);
                s.extend(chars.iter().skip(pos));
                s
            }
        }
        "BEFORE" => {
            let sep = arg(0);
            let nth: usize = arg(1).parse().unwrap_or(1);
            let mut idx = None;
            let mut from = 0;
            let mut seen = 0;
            while let Some(p) = text[from..].find(sep.as_str()) {
                seen += 1;
                if seen == nth {
                    idx = Some(from + p);
                    break;
                }
                from += p + sep.len();
            }
            match idx {
                Some(i) => text[..i].to_string(),
                None => text.to_string(),
            }
        }
        "AFTER" => {
            let sep = arg(0);
            let nth: usize = arg(1).parse().unwrap_or(1);
            let mut from = 0;
            let mut seen = 0;
            let mut out = text.to_string();
            while let Some(p) = out[from..].find(sep.as_str()) {
                seen += 1;
                if seen == nth {
                    let i = from + p;
                    return out[i + sep.len()..].to_string();
                }
                from += p + sep.len();
            }
            out
        }
        "BETWEEN" => {
            let a = arg(0);
            let b = arg(1);
            match text.find(a.as_str()) {
                Some(i) => {
                    let start = i + a.len();
                    match text[start..].find(b.as_str()) {
                        Some(j) => text[start..start + j].to_string(),
                        None => text[start..].to_string(),
                    }
                }
                None => match text.find(b.as_str()) {
                    Some(j) => text[..j].to_string(),
                    None => text.to_string(),
                },
            }
        }
        "BETWEEN_DEL" => {
            let a = arg(0);
            let b = arg(1);
            match (text.find(a.as_str()), text.find(b.as_str())) {
                (Some(i), Some(j)) if j > i => {
                    format!("{}{}", &text[..i], &text[j + b.len()..])
                }
                _ => text.to_string(),
            }
        }
        "REPLACE" => {
            let old = arg(0);
            let new = arg(1);
            if old.is_empty() {
                return text.to_string();
            }
            if args.len() > 2 && !args[2].is_empty() {
                let p = args[2];
                if let Some(num) = p.parse::<usize>().ok() {
                    text.replacen(old.as_str(), &new, num)
                } else {
                    text.replace(old.as_str(), &new)
                }
            } else {
                text.replace(old.as_str(), &new)
            }
        }
        "RE" => match regex_lite::Regex::new(rest) {
            Ok(re) => re.find_iter(text).map(|m| m.as_str()).collect(),
            Err(_) => text.to_string(),
        },
        "REP_RE" | "REPLACE_RE" => {
            let pat = arg(0);
            let rep = arg(1);
            match regex_lite::Regex::new(&pat) {
                Ok(re) => re.replace_all(text, rep.as_str()).to_string(),
                Err(_) => text.to_string(),
            }
        }
        "WILDCARD" => {
            let pat = rest.replace('*', ".*").replace('?', ".");
            match regex_lite::Regex::new(&format!("^(?:{})$", pat)) {
                Ok(re) => {
                    if re.is_match(text) {
                        text.to_string()
                    } else {
                        String::new()
                    }
                }
                Err(_) => text.to_string(),
            }
        }
        "MATH" => match eval_math(rest, text) {
            Some(v) => {
                if (v - v.round()).abs() < 1e-9 {
                    format!("{}", v.round() as i64)
                } else {
                    format!("{:.6}", v).trim_end_matches('0').trim_end_matches('.').to_string()
                }
            }
            None => text.to_string(),
        },
        "NU2CN" => text
            .parse::<i64>()
            .map(num_to_cn)
            .unwrap_or_else(|_| text.to_string()),
        "NU2EN" => text
            .parse::<i64>()
            .map(num_to_en)
            .unwrap_or_else(|_| text.to_string()),
        "NU2RM" => text
            .parse::<i64>()
            .map(num_to_rm)
            .unwrap_or_else(|_| text.to_string()),
        "NU2ABC" => text
            .parse::<i64>()
            .map(num_to_abc)
            .unwrap_or_else(|_| text.to_string()),
        "CN2NU" => cn_to_num(text)
            .map(|v| v.to_string())
            .unwrap_or_else(|| text.to_string()),
        "EN2NU" => en_to_num(text)
            .map(|v| v.to_string())
            .unwrap_or_else(|| text.to_string()),
        "ABC2NU" => abc_to_num(text)
            .map(|v| v.to_string())
            .unwrap_or_else(|| text.to_string()),
        "RM2NU" => rm_to_num(text)
            .map(|v| v.to_string())
            .unwrap_or_else(|| text.to_string()),
        "NU2TS" => text
            .parse::<i64>()
            .map(|v| format!("{}", v))
            .unwrap_or_else(|_| text.to_string()),
        "TS2NU" => text
            .parse::<i64>()
            .map(|v| date_part("DATE", v))
            .unwrap_or_else(|_| text.to_string()),
        "HASH" => {
            let mut h: u64 = 0xcbf29ce484222325;
            for b in text.as_bytes() {
                h ^= *b as u64;
                h = h.wrapping_mul(0x100000001b3);
            }
            format!("{:x}", h)
        }
        "BASE64_ENCODE" => base64_encode(text),
        "BASE64_DECODE" => base64_decode(text).unwrap_or_else(|| text.to_string()),
        _ => text.to_string(),
    }
}

fn cut_text(chars: &[char], param: &str) -> String {
    // 裸写的 a-b 区间（兼容插件里需要 [a-b] 的写法）
    if !param.starts_with('-') {
        if let Some((a, b)) = param.split_once('-') {
            if let (Ok(a), Ok(b)) = (a.parse::<usize>(), b.parse::<usize>()) {
                let lo = a.saturating_sub(1).min(chars.len());
                let hi = b.min(chars.len());
                return if lo < hi { chars[lo..hi].iter().collect() } else { String::new() };
            }
        }
    }
    if let Some(rest) = param.strip_prefix('-') {
        let a: usize = rest.parse().unwrap_or(0);
        let n = chars.len();
        return chars[n.saturating_sub(a)..].iter().collect();
    }
    if param.starts_with('[') && param.ends_with(']') {
        let idx = &param[1..param.len() - 1];
        if let Some((a, b)) = idx.split_once('-') {
            let a: usize = a.parse().unwrap_or(1);
            let b: usize = b.parse().unwrap_or(chars.len());
            let lo = a.saturating_sub(1).min(chars.len());
            let hi = b.min(chars.len());
            if lo < hi {
                return chars[lo..hi].iter().collect();
            }
            return String::new();
        }
        let a: usize = idx.parse().unwrap_or(0);
        return chars
            .get(a.saturating_sub(1))
            .map(|c| c.to_string())
            .unwrap_or_default();
    }
    let a: usize = param.parse().unwrap_or(0);
    chars.iter().take(a).collect()
}

fn del_text(chars: &[char], param: &str) -> String {
    if !param.starts_with('-') {
        if let Some((a, b)) = param.split_once('-') {
            if let (Ok(a), Ok(b)) = (a.parse::<usize>(), b.parse::<usize>()) {
                let lo = a.saturating_sub(1).min(chars.len());
                let hi = b.min(chars.len());
                let mut out: String = chars[..lo].iter().collect();
                out.extend(chars[hi..].iter());
                return out;
            }
        }
    }
    if let Some(rest) = param.strip_prefix('-') {
        let a: usize = rest.parse().unwrap_or(0);
        let n = chars.len();
        if a >= n {
            return String::new();
        }
        return chars[..n - a].iter().collect();
    }
    if param.starts_with('[') && param.ends_with(']') {
        let idx = &param[1..param.len() - 1];
        if let Some((a, b)) = idx.split_once('-') {
            let a: usize = a.parse().unwrap_or(1);
            let b: usize = b.parse().unwrap_or(1);
            let lo = a.saturating_sub(1).min(chars.len());
            let hi = b.min(chars.len());
            let mut out: String = chars[..lo].iter().collect();
            out.extend(chars[hi..].iter());
            return out;
        }
        let a: usize = idx.parse().unwrap_or(0);
        let mut out: String = chars.iter().take(a.saturating_sub(1)).collect();
        out.extend(chars.iter().skip(a));
        return out;
    }
    let a: usize = param.parse().unwrap_or(0);
    chars.iter().skip(a).collect()
}

fn pad_text(text: &str, x: &str, y: &str) -> String {
    let Ok(xv) = x.parse::<i64>() else {
        return text.to_string();
    };
    let fill = if y.is_empty() { "0" } else { y };
    let abs = xv.unsigned_abs() as usize;
    let chars: Vec<char> = text.chars().collect();
    if chars.len() > abs {
        return if xv < 0 {
            chars[..abs].iter().collect()
        } else {
            chars[chars.len() - abs..].iter().collect()
        };
    }
    if chars.len() < abs {
        let need = abs - chars.len();
        let mut padding = String::new();
        while padding.chars().count() < need {
            padding.push_str(fill);
        }
        let padding: String = padding.chars().take(need).collect();
        return if xv < 0 {
            format!("{}{}", text, padding)
        } else {
            format!("{}{}", padding, text)
        };
    }
    text.to_string()
}

// ---------------------------------------------------------------- 序号转换

fn num_to_cn(n: i64) -> String {
    const D: [&str; 10] = ["零", "一", "二", "三", "四", "五", "六", "七", "八", "九"];
    const U: [&str; 4] = ["", "十", "百", "千"];
    if n == 0 {
        return "零".into();
    }
    if n < 0 {
        return format!("负{}", num_to_cn(-n));
    }
    if n >= 10000 {
        let high = n / 10000;
        let low = n % 10000;
        let mut s = format!("{}万", num_to_cn(high));
        if low > 0 {
            if low < 1000 {
                s.push('零');
            }
            s.push_str(&num_to_cn(low));
        }
        return s;
    }
    let digits: Vec<i64> = n.to_string().chars().map(|c| c as i64 - 48).collect();
    let mut out = String::new();
    let mut zero = false;
    for (i, d) in digits.iter().enumerate() {
        let unit = digits.len() - 1 - i;
        if *d == 0 {
            zero = true;
        } else {
            if zero && !out.is_empty() {
                out.push('零');
            }
            zero = false;
            out.push_str(D[*d as usize]);
            out.push_str(U[unit]);
        }
    }
    // 十X 读作“十X”而非“一十X”
    if out.starts_with('一') && digits.len() == 2 && digits[0] == 1 {
        out = out[3..].to_string();
        out.insert(0, '十');
    }
    out
}

fn num_to_en(n: i64) -> String {
    const ONES: [&str; 20] = [
        "zero", "one", "two", "three", "four", "five", "six", "seven", "eight", "nine", "ten",
        "eleven", "twelve", "thirteen", "fourteen", "fifteen", "sixteen", "seventeen",
        "eighteen", "nineteen",
    ];
    const TENS: [&str; 10] = [
        "", "", "twenty", "thirty", "forty", "fifty", "sixty", "seventy", "eighty", "ninety",
    ];
    if n < 0 {
        return format!("minus {}", num_to_en(-n));
    }
    if n < 20 {
        return ONES[n as usize].into();
    }
    if n < 100 {
        let t = TENS[(n / 10) as usize];
        let r = n % 10;
        return if r == 0 {
            t.into()
        } else {
            format!("{}-{}", t, ONES[r as usize])
        };
    }
    if n < 1000 {
        let h = format!("{} hundred", ONES[(n / 100) as usize]);
        let r = n % 100;
        return if r == 0 {
            h
        } else {
            format!("{} {}", h, num_to_en(r))
        };
    }
    if n < 1_000_000 {
        let th = format!("{} thousand", num_to_en(n / 1000));
        let r = n % 1000;
        return if r == 0 {
            th
        } else {
            format!("{} {}", th, num_to_en(r))
        };
    }
    format!("{}", n)
}

fn num_to_rm(n: i64) -> String {
    if n <= 0 || n > 3999 {
        return n.to_string();
    }
    const V: [(i64, &str); 13] = [
        (1000, "M"),
        (900, "CM"),
        (500, "D"),
        (400, "CD"),
        (100, "C"),
        (90, "XC"),
        (50, "L"),
        (40, "XL"),
        (10, "X"),
        (9, "IX"),
        (5, "V"),
        (4, "IV"),
        (1, "I"),
    ];
    let mut v = n;
    let mut out = String::new();
    for (val, sym) in V {
        while v >= val {
            out.push_str(sym);
            v -= val;
        }
    }
    out
}

fn num_to_abc(n: i64) -> String {
    if n <= 0 {
        return String::new();
    }
    let mut v = n;
    let mut out = String::new();
    while v > 0 {
        let r = ((v - 1) % 26) as u8;
        out.insert(0, (b'A' + r) as char);
        v = (v - 1) / 26;
    }
    out
}

fn abc_to_num(s: &str) -> Option<i64> {
    if s.is_empty() {
        return None;
    }
    let mut v: i64 = 0;
    for c in s.chars() {
        if !c.is_ascii_alphabetic() {
            return None;
        }
        v = v * 26 + (c.to_ascii_uppercase() as i64 - 'A' as i64 + 1);
    }
    Some(v)
}

fn cn_to_num(s: &str) -> Option<i64> {
    let digits = ['零', '一', '二', '三', '四', '五', '六', '七', '八', '九'];
    let units = ['十', '百', '千', '万'];
    let mut total = 0i64;
    let mut section = 0i64;
    let mut last = 0i64;
    for c in s.chars() {
        if let Some(d) = digits.iter().position(|&x| x == c) {
            last = d as i64;
        } else if let Some(u) = units.iter().position(|&x| x == c) {
            let mult = match u {
                0 => 10,
                1 => 100,
                2 => 1000,
                _ => 10000,
            };
            if mult == 10000 {
                total = (section + last) * 10000;
                section = 0;
            } else {
                section += if last == 0 { 1 } else { last } * mult;
            }
            last = 0;
        } else {
            return None;
        }
    }
    Some(total + section + last)
}

fn en_to_num(s: &str) -> Option<i64> {
    let mut total = 0i64;
    let mut cur = 0i64;
    let mut any = false;
    for w in s.to_lowercase().split(|c: char| c == ' ' || c == '-' || c == ',') {
        if w.is_empty() {
            continue;
        }
        any = true;
        let v = match w {
            "zero" => 0,
            "one" => 1,
            "two" => 2,
            "three" => 3,
            "four" => 4,
            "five" => 5,
            "six" => 6,
            "seven" => 7,
            "eight" => 8,
            "nine" => 9,
            "ten" => 10,
            "eleven" => 11,
            "twelve" => 12,
            "thirteen" => 13,
            "fourteen" => 14,
            "fifteen" => 15,
            "sixteen" => 16,
            "seventeen" => 17,
            "eighteen" => 18,
            "nineteen" => 19,
            "twenty" => 20,
            "thirty" => 30,
            "forty" => 40,
            "fifty" => 50,
            "sixty" => 60,
            "seventy" => 70,
            "eighty" => 80,
            "ninety" => 90,
            "hundred" => {
                cur *= 100;
                continue;
            }
            "thousand" => {
                total += cur * 1000;
                cur = 0;
                continue;
            }
            "million" => {
                total += cur * 1_000_000;
                cur = 0;
                continue;
            }
            _ => return None,
        };
        cur += v;
    }
    if !any {
        return None;
    }
    Some(total + cur)
}

fn rm_to_num(s: &str) -> Option<i64> {
    let up = s.to_uppercase();
    let mut total = 0i64;
    let val = |c: char| match c {
        'I' => 1,
        'V' => 5,
        'X' => 10,
        'L' => 50,
        'C' => 100,
        'D' => 500,
        'M' => 1000,
        _ => 0,
    };
    let chars: Vec<char> = up.chars().collect();
    if chars.is_empty() || chars.iter().any(|c| val(*c) == 0) {
        return None;
    }
    for i in 0..chars.len() {
        let v = val(chars[i]);
        if i + 1 < chars.len() && v < val(chars[i + 1]) {
            total -= v;
        } else {
            total += v;
        }
    }
    Some(total)
}

pub(crate) fn base64_encode(s: &str) -> String {
    const T: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let bytes = s.as_bytes();
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            T[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            T[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

fn base64_decode(s: &str) -> Option<String> {
    const T: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut buf: u32 = 0;
    let mut bits = 0;
    let mut out = Vec::new();
    for c in s.bytes() {
        if c == b'=' {
            break;
        }
        let Some(v) = T.iter().position(|&x| x == c) else {
            return None;
        };
        buf = (buf << 6) | v as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buf >> bits) as u8);
        }
    }
    String::from_utf8(out).ok()
}

// ---------------------------------------------------------------- MATH

fn eval_math(expr: &str, self_val: &str) -> Option<f64> {
    let self_num = self_val.trim().parse::<f64>().ok();
    let tokens: Vec<char> = expr.chars().filter(|c| !c.is_whitespace()).collect();
    let mut pos = 0usize;
    let v = parse_expr(&tokens, &mut pos, self_num)?;
    if pos < tokens.len() {
        return None;
    }
    Some(v)
}

fn parse_expr(t: &[char], pos: &mut usize, self_num: Option<f64>) -> Option<f64> {
    let mut v = parse_term(t, pos, self_num)?;
    while *pos < t.len() {
        match t[*pos] {
            '+' => {
                *pos += 1;
                v += parse_term(t, pos, self_num)?;
            }
            '-' => {
                *pos += 1;
                v -= parse_term(t, pos, self_num)?;
            }
            _ => break,
        }
    }
    Some(v)
}

fn parse_term(t: &[char], pos: &mut usize, self_num: Option<f64>) -> Option<f64> {
    let mut v = parse_pow(t, pos, self_num)?;
    while *pos < t.len() {
        match t[*pos] {
            '*' => {
                if t.get(*pos + 1) == Some(&'*') {
                    break;
                }
                *pos += 1;
                v *= parse_pow(t, pos, self_num)?;
            }
            '%' => {
                if t.get(*pos + 1) == Some(&'%') {
                    *pos += 2;
                    let d = parse_pow(t, pos, self_num)?;
                    if d == 0.0 {
                        return None;
                    }
                    v %= d;
                } else {
                    *pos += 1;
                    let d = parse_pow(t, pos, self_num)?;
                    if d == 0.0 {
                        return None;
                    }
                    v /= d;
                }
            }
            _ => break,
        }
    }
    Some(v)
}

fn parse_pow(t: &[char], pos: &mut usize, self_num: Option<f64>) -> Option<f64> {
    let base = parse_atom(t, pos, self_num)?;
    if t.get(*pos) == Some(&'*') && t.get(*pos + 1) == Some(&'*') {
        *pos += 2;
        let e = parse_pow(t, pos, self_num)?;
        return Some(base.powf(e));
    }
    Some(base)
}

fn parse_atom(t: &[char], pos: &mut usize, self_num: Option<f64>) -> Option<f64> {
    if *pos >= t.len() {
        return None;
    }
    match t[*pos] {
        '(' => {
            *pos += 1;
            let v = parse_expr(t, pos, self_num)?;
            if t.get(*pos) == Some(&')') {
                *pos += 1;
            }
            Some(v)
        }
        '-' => {
            *pos += 1;
            Some(-parse_atom(t, pos, self_num)?)
        }
        '+' => {
            *pos += 1;
            parse_atom(t, pos, self_num)
        }
        _ => {
            // 数字
            let start = *pos;
            while *pos < t.len() && (t[*pos].is_ascii_digit() || t[*pos] == '.') {
                *pos += 1;
            }
            if *pos > start {
                let s: String = t[start..*pos].iter().collect();
                return s.parse::<f64>().ok();
            }
            // 标识符或函数
            let start = *pos;
            while *pos < t.len() && (t[*pos].is_ascii_alphanumeric() || t[*pos] == '_') {
                *pos += 1;
            }
            let name: String = t[start..*pos].iter().collect::<String>().to_lowercase();
            if name.is_empty() {
                return None;
            }
            if t.get(*pos) == Some(&'(') {
                *pos += 1;
                let mut a = Vec::new();
                if t.get(*pos) != Some(&')') {
                    loop {
                        a.push(parse_expr(t, pos, self_num)?);
                        if t.get(*pos) == Some(&',') {
                            *pos += 1;
                            continue;
                        }
                        break;
                    }
                }
                if t.get(*pos) == Some(&')') {
                    *pos += 1;
                }
                let r = match name.as_str() {
                    "sqrt" => a.first()?.sqrt(),
                    "abs" | "fabs" => a.first()?.abs(),
                    "floor" => a.first()?.floor(),
                    "ceil" => a.first()?.ceil(),
                    "trunc" => a.first()?.trunc(),
                    "round" => {
                        let d = a.get(1).copied().unwrap_or(0.0) as i32;
                        let m = 10f64.powi(d);
                        (a.first()? * m).round() / m
                    }
                    "sin" => a.first()?.sin(),
                    "cos" => a.first()?.cos(),
                    "tan" => a.first()?.tan(),
                    "asin" => a.first()?.asin(),
                    "acos" => a.first()?.acos(),
                    "atan" => a.first()?.atan(),
                    "exp" => a.first()?.exp(),
                    "log" => {
                        let x = *a.first()?;
                        match a.get(1) {
                            Some(b) => x.log(*b),
                            None => x.ln(),
                        }
                    }
                    "log10" => a.first()?.log10(),
                    "log2" => a.first()?.log2(),
                    "pow" => a.first()?.powf(*a.get(1)?),
                    "max" => a.first()?.max(*a.get(1)?),
                    "min" => a.first()?.min(*a.get(1)?),
                    "sgn" => {
                        let x = *a.first()?;
                        if x > 0.0 {
                            1.0
                        } else if x < 0.0 {
                            -1.0
                        } else {
                            0.0
                        }
                    }
                    "degrees" => a.first()?.to_degrees(),
                    "radians" => a.first()?.to_radians(),
                    _ => return None,
                };
                return Some(r);
            }
            match name.as_str() {
                "pi" => Some(std::f64::consts::PI),
                "e" => Some(std::f64::consts::E),
                "self" => self_num,
                _ => None,
            }
        }
    }
}

/// 对字节做 base64（供预览 data URL 用）
pub(crate) fn base64_encode_bytes(bytes: &[u8]) -> String {
    const T: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 { T[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if chunk.len() > 2 { T[n as usize & 63] as char } else { '=' });
    }
    out
}
