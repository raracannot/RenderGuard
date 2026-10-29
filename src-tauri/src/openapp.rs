//! 找出当前开着的 Blender 窗口，并解析它打开的 .blend。
//!
//! Blender（5.x）的窗口标题形如：
//!   scene [E:\proj\scene.blend] - Blender 5.1.0
//! 方括号里就是完整路径，所以不必去猜 recent-files。
//! 老版本或未保存的文件只有文件名，此时 file 为 None，由前端提示手动指定。

use std::cell::RefCell;
use std::ffi::c_void;

use serde::Serialize;

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct OpenBlender {
    pub pid: u32,
    pub title: String,
    pub exe: String,
    pub name: String,
    pub file: Option<String>,
}

type Hwnd = *mut c_void;

#[link(name = "user32")]
extern "system" {
    fn EnumWindows(cb: extern "system" fn(Hwnd, isize) -> i32, lparam: isize) -> i32;
    fn IsWindowVisible(h: Hwnd) -> i32;
    fn GetWindowTextW(h: Hwnd, buf: *mut u16, len: i32) -> i32;
    fn GetWindowTextLengthW(h: Hwnd) -> i32;
    fn GetWindowThreadProcessId(h: Hwnd, pid: *mut u32) -> u32;
}

#[link(name = "kernel32")]
extern "system" {
    fn OpenProcess(access: u32, inherit: i32, pid: u32) -> *mut c_void;
    fn CloseHandle(h: *mut c_void) -> i32;
    fn QueryFullProcessImageNameW(h: *mut c_void, flags: u32, buf: *mut u16, size: *mut u32) -> i32;
}

const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;

thread_local! {
    static FOUND: RefCell<Vec<(u32, String)>> = const { RefCell::new(Vec::new()) };
}

extern "system" fn enum_cb(h: Hwnd, _l: isize) -> i32 {
    unsafe {
        if IsWindowVisible(h) == 0 {
            return 1;
        }
        let len = GetWindowTextLengthW(h);
        if len <= 0 {
            return 1;
        }
        let mut buf = vec![0u16; (len + 1) as usize];
        let n = GetWindowTextW(h, buf.as_mut_ptr(), buf.len() as i32);
        if n <= 0 {
            return 1;
        }
        buf.truncate(n as usize);
        let title = String::from_utf16_lossy(&buf);
        if !title.contains("Blender") {
            return 1;
        }
        let mut pid: u32 = 0;
        GetWindowThreadProcessId(h, &mut pid);
        if pid != 0 {
            FOUND.with(|f| f.borrow_mut().push((pid, title)));
        }
    }
    1
}

fn process_image(pid: u32) -> Option<String> {
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if h.is_null() {
            return None;
        }
        let mut buf = vec![0u16; 2048];
        let mut size = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(h, 0, buf.as_mut_ptr(), &mut size);
        CloseHandle(h);
        if ok == 0 || size == 0 {
            return None;
        }
        Some(String::from_utf16_lossy(&buf[..size as usize]))
    }
}

/// "scene [E:\a\b.blend] - Blender 5.1.0" → ("scene.blend", Some("E:\a\b.blend"))
fn parse_title(title: &str) -> (String, Option<String>) {
    if let (Some(l), Some(r)) = (title.find('['), title.rfind(']')) {
        if r > l {
            let path = title[l + 1..r].trim().to_string();
            let name = path
                .rsplit(['\\', '/'])
                .next()
                .unwrap_or(&path)
                .to_string();
            if path.to_ascii_lowercase().ends_with(".blend") {
                return (name, Some(path));
            }
            return (name, None);
        }
    }
    // 未保存 / 老版本：只有文件名
    let head = title.split(" - Blender").next().unwrap_or(title).trim();
    let head = head.trim_end_matches('*').trim();
    let name = head
        .rsplit(['\\', '/'])
        .next()
        .unwrap_or(head)
        .to_string();
    (name, None)
}

pub fn list() -> Vec<OpenBlender> {
    FOUND.with(|f| f.borrow_mut().clear());
    unsafe {
        EnumWindows(enum_cb, 0);
    }

    let raw: Vec<(u32, String)> = FOUND.with(|f| f.borrow().clone());
    let mut out: Vec<OpenBlender> = Vec::new();
    let mut seen: Vec<u32> = Vec::new();

    for (pid, title) in raw {
        if seen.contains(&pid) {
            continue;
        }
        let Some(exe) = process_image(pid) else { continue };
        if !exe.to_ascii_lowercase().ends_with("blender.exe") {
            continue;
        }
        let (name, file) = parse_title(&title);
        seen.push(pid);
        out.push(OpenBlender {
            pid,
            title,
            exe,
            name,
            file,
        });
    }
    out
}
