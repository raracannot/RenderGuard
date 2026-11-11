//! 视频元信息：直接加载 Blender 自带的 avformat/avutil DLL，不启动 Blender 进程。
//!
//! 不碰 AVFormatContext 的结构布局（各版本差异大），只调用稳定导出函数，
//! 通过 av_log_format_line2 把 av_dump_format 的输出抓成文本再解析。

#![allow(non_snake_case)]

use std::ffi::{c_char, c_int, c_void};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

#[derive(Clone, Debug, Default)]
pub struct VideoInfo {
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    pub duration: f64,
}

type Handle = *mut c_void;

#[link(name = "kernel32")]
extern "system" {
    fn LoadLibraryExW(name: *const u16, file: *mut c_void, flags: u32) -> Handle;
    fn GetProcAddress(module: Handle, name: *const u8) -> *mut c_void;
}

const LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR: u32 = 0x0000_0100;
const LOAD_LIBRARY_SEARCH_DEFAULT_DIRS: u32 = 0x0000_1000;

type FnOpenInput = unsafe extern "system" fn(*mut Handle, *const c_char, *mut c_void, *mut c_void) -> c_int;
type FnFindStream = unsafe extern "system" fn(Handle, *mut c_void) -> c_int;
type FnCloseInput = unsafe extern "system" fn(*mut Handle);
type FnDumpFormat = unsafe extern "system" fn(Handle, c_int, *const c_char, c_int);
type FnSetLevel = unsafe extern "system" fn(c_int);
type FnSetCallback = unsafe extern "system" fn(*const c_void);
type FnFormatLine = unsafe extern "system" fn(
    *mut c_void,
    c_int,
    *const c_char,
    *mut c_void,
    *mut c_char,
    c_int,
    *mut c_int,
) -> c_int;

struct Lib {
    open_input: FnOpenInput,
    find_stream: FnFindStream,
    close_input: FnCloseInput,
    dump_format: FnDumpFormat,
    set_level: FnSetLevel,
    set_callback: FnSetCallback,
}

unsafe impl Send for Lib {}
unsafe impl Sync for Lib {}

static LIB: OnceLock<Option<Lib>> = OnceLock::new();
static mut FORMAT_LINE: Option<FnFormatLine> = None;

thread_local! {
    static LINES: std::cell::RefCell<Vec<String>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// av_log 回调：把一条日志格式化后收集起来
unsafe extern "system" fn log_cb(
    opaque: *mut c_void,
    level: c_int,
    fmt: *const c_char,
    va: *mut c_void,
) {
    if fmt.is_null() {
        return;
    }
    let Some(line_fn) = (unsafe { FORMAT_LINE }) else {
        return;
    };
    let mut buf = [0i8; 2048];
    let mut prefix: c_int = 1;
    let n = unsafe {
        line_fn(
            opaque,
            level,
            fmt,
            va,
            buf.as_mut_ptr(),
            buf.len() as c_int,
            &mut prefix,
        )
    };
    if n > 0 {
        let cstr = unsafe { std::ffi::CStr::from_ptr(buf.as_ptr()) };
        if let Ok(s) = cstr.to_str() {
            LINES.with(|l| l.borrow_mut().push(s.to_string()));
        }
    }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn find_dll(dir: &Path, prefix: &str) -> Option<PathBuf> {
    let rd = std::fs::read_dir(dir).ok()?;
    let mut best: Option<PathBuf> = None;
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        if name.starts_with(prefix) && name.ends_with(".dll") {
            best = Some(e.path());
        }
    }
    best
}

unsafe fn sym(module: Handle, name: &str) -> Option<*mut c_void> {
    let n = format!("{}\0", name);
    let p = unsafe { GetProcAddress(module, n.as_ptr()) };
    if p.is_null() {
        None
    } else {
        Some(p)
    }
}

/// blender.shared 目录：从 Blender 可执行文件路径推出来
pub fn shared_dir_of(blender: &str) -> Option<PathBuf> {
    let exe = Path::new(blender);
    let root = exe.parent()?;
    let shared = root.join("blender.shared");
    if shared.is_dir() {
        return Some(shared);
    }
    // 少数版本把 DLL 放在根目录
    if find_dll(root, "avformat-").is_some() {
        return Some(root.to_path_buf());
    }
    None
}

fn load_lib(blender: &str) -> Option<Lib> {
    let dir = shared_dir_of(blender)?;
    let avformat = find_dll(&dir, "avformat-")?;
    let avutil = find_dll(&dir, "avutil-")?;

    unsafe {
        let flags = LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_DEFAULT_DIRS;
        let h_util = LoadLibraryExW(wide(avutil.to_string_lossy().as_ref()).as_ptr(), std::ptr::null_mut(), flags);
        if h_util.is_null() {
            return None;
        }
        let h_fmt = LoadLibraryExW(wide(avformat.to_string_lossy().as_ref()).as_ptr(), std::ptr::null_mut(), flags);
        if h_fmt.is_null() {
            return None;
        }

        let lib = Lib {
            open_input: std::mem::transmute::<*mut c_void, FnOpenInput>(sym(h_fmt, "avformat_open_input")?),
            find_stream: std::mem::transmute::<*mut c_void, FnFindStream>(sym(h_fmt, "avformat_find_stream_info")?),
            close_input: std::mem::transmute::<*mut c_void, FnCloseInput>(sym(h_fmt, "avformat_close_input")?),
            dump_format: std::mem::transmute::<*mut c_void, FnDumpFormat>(sym(h_fmt, "av_dump_format")?),
            set_level: std::mem::transmute::<*mut c_void, FnSetLevel>(sym(h_util, "av_log_set_level")?),
            set_callback: std::mem::transmute::<*mut c_void, FnSetCallback>(sym(h_util, "av_log_set_callback")?),
        };
        FORMAT_LINE = Some(std::mem::transmute::<*mut c_void, FnFormatLine>(sym(
            h_util,
            "av_log_format_line2",
        )?));
        Some(lib)
    }
}

fn lib_for(blender: &str) -> Option<&'static Lib> {
    LIB.get_or_init(|| load_lib(blender)).as_ref()
}

/// 读视频信息；DLL 不可用时返回 None（调用方降级）
pub fn probe(blender: &str, path: &str) -> Option<VideoInfo> {
    let lib = lib_for(blender)?;
    let cpath = std::ffi::CString::new(path).ok()?;

    unsafe {
        (lib.set_level)(32);
        (lib.set_callback)(log_cb as *const c_void);
        LINES.with(|l| l.borrow_mut().clear());

        let mut ctx: Handle = std::ptr::null_mut();
        let rc = (lib.open_input)(&mut ctx, cpath.as_ptr(), std::ptr::null_mut(), std::ptr::null_mut());
        if rc != 0 {
            return None;
        }
        (lib.find_stream)(ctx, std::ptr::null_mut());
        (lib.dump_format)(ctx, 0, cpath.as_ptr(), 0);
        (lib.close_input)(&mut ctx);
    }

    let text = LINES.with(|l| l.borrow().join(""));
    parse(&text)
}

fn parse(text: &str) -> Option<VideoInfo> {
    if text.is_empty() {
        return None;
    }
    let mut info = VideoInfo::default();

    if let Some(i) = text.find("Duration:") {
        let rest = &text[i + 9..];
        let head: String = rest.chars().take(16).collect();
        let parts: Vec<&str> = head.trim().split(':').collect();
        if parts.len() >= 3 {
            let h: f64 = parts[0].trim().parse().unwrap_or(0.0);
            let m: f64 = parts[1].parse().unwrap_or(0.0);
            let s: f64 = parts[2].split(|c: char| !c.is_ascii_digit() && c != '.').next()
                .unwrap_or("0").parse().unwrap_or(0.0);
            info.duration = h * 3600.0 + m * 60.0 + s;
        }
    }

    for line in text.lines() {
        if !line.contains("Stream #") || !line.contains("Video:") {
            continue;
        }
        // 分辨率：第一个 数字x数字
        let bytes = line.as_bytes();
        let mut i = 0;
        while i + 2 < bytes.len() {
            if bytes[i].is_ascii_digit() {
                let start = i;
                while i < bytes.len() && bytes[i].is_ascii_digit() {
                    i += 1;
                }
                if i < bytes.len() && (bytes[i] == b'x' || bytes[i] == b'X') {
                    let w: String = line[start..i].to_string();
                    let mut j = i + 1;
                    while j < bytes.len() && bytes[j].is_ascii_digit() {
                        j += 1;
                    }
                    let hstr: String = line[i + 1..j].to_string();
                    if let (Ok(w), Ok(h)) = (w.parse::<u32>(), hstr.parse::<u32>()) {
                        if w >= 16 && h >= 16 {
                            info.width = w;
                            info.height = h;
                            break;
                        }
                    }
                }
                continue;
            }
            i += 1;
        }
        // 帧率：\d+(\.\d+)? fps
        if let Some(p) = line.find(" fps") {
            let before = &line[..p];
            let start = before
                .rfind(|c: char| !c.is_ascii_digit() && c != '.')
                .map(|i| i + 1)
                .unwrap_or(0);
            if let Ok(v) = before[start..].parse::<f64>() {
                info.fps = v;
            }
        }
        break;
    }

    if info.width == 0 && info.duration == 0.0 {
        None
    } else {
        Some(info)
    }
}

/// 读设置里的 Blender 路径，自动探测（结果缓存）
pub fn probe_auto(path: &str) -> Option<VideoInfo> {
    static BLENDER: OnceLock<String> = OnceLock::new();
    let blender = BLENDER.get_or_init(|| {
        crate::store::load_settings()
            .default_blender
            .trim()
            .to_string()
    });
    if blender.is_empty() {
        return None;
    }
    probe(blender, path)
}
