//! Windows 进程底层：CPU 时间、结束进程树、Job Object（父进程死了子进程一起死）。
#![allow(non_snake_case)]

use std::ffi::c_void;
use std::sync::OnceLock;

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
const PROCESS_SET_QUOTA: u32 = 0x0100;
const PROCESS_TERMINATE: u32 = 0x0001;

const JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE: u32 = 0x2000;
const JOB_OBJECT_EXTENDED_LIMIT_INFORMATION: i32 = 9;

#[repr(C)]
#[derive(Default, Clone, Copy)]
pub struct FILETIME {
    dwLowDateTime: u32,
    dwHighDateTime: u32,
}

#[repr(C)]
#[derive(Default)]
struct JOBOBJECT_BASIC_LIMIT_INFORMATION {
    PerProcessUserTimeLimit: i64,
    PerJobUserTimeLimit: i64,
    LimitFlags: u32,
    MinimumWorkingSetSize: usize,
    MaximumWorkingSetSize: usize,
    ActiveProcessLimit: u32,
    Affinity: usize,
    PriorityClass: u32,
    SchedulingClass: u32,
}

#[repr(C)]
#[derive(Default)]
struct IO_COUNTERS {
    ReadOperationCount: u64,
    WriteOperationCount: u64,
    OtherOperationCount: u64,
    ReadTransferCount: u64,
    WriteTransferCount: u64,
    OtherTransferCount: u64,
}

#[repr(C)]
#[derive(Default)]
struct JOBOBJECT_EXTENDED_LIMIT_INFORMATION {
    BasicLimitInformation: JOBOBJECT_BASIC_LIMIT_INFORMATION,
    IoInfo: IO_COUNTERS,
    ProcessMemoryLimit: usize,
    JobMemoryLimit: usize,
    PeakProcessMemoryUsed: usize,
    PeakJobMemoryUsed: usize,
}

#[link(name = "kernel32")]
extern "system" {
    fn OpenProcess(access: u32, inherit: i32, pid: u32) -> *mut c_void;
    fn CloseHandle(h: *mut c_void) -> i32;
    fn GetProcessTimes(
        h: *mut c_void,
        creation: *mut FILETIME,
        exit: *mut FILETIME,
        kernel: *mut FILETIME,
        user: *mut FILETIME,
    ) -> i32;
    fn CreateJobObjectW(attrs: *mut c_void, name: *const u16) -> *mut c_void;
    fn SetInformationJobObject(job: *mut c_void, class: i32, info: *mut c_void, len: u32) -> i32;
    fn AssignProcessToJobObject(job: *mut c_void, process: *mut c_void) -> i32;
}

static JOB: OnceLock<isize> = OnceLock::new();

/// 创建一个"句柄关闭即杀死所有成员进程"的 Job Object。
/// 这样即使本程序被强杀，渲染中的 Blender 也不会变成孤儿进程继续吃显卡。
pub fn init_kill_on_close_job() {
    unsafe {
        let h = CreateJobObjectW(std::ptr::null_mut(), std::ptr::null());
        if h.is_null() {
            return;
        }
        let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        let ok = SetInformationJobObject(
            h,
            JOB_OBJECT_EXTENDED_LIMIT_INFORMATION,
            &mut info as *mut _ as *mut c_void,
            std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        );
        if ok == 0 {
            return;
        }
        let _ = JOB.set(h as isize);
    }
}

/// 把子进程收编进 Job Object
pub fn attach_to_job(pid: u32) {
    let Some(&jh) = JOB.get() else { return };
    unsafe {
        let ph = OpenProcess(PROCESS_SET_QUOTA | PROCESS_TERMINATE, 0, pid);
        if ph.is_null() {
            return;
        }
        AssignProcessToJobObject(jh as *mut c_void, ph);
        CloseHandle(ph);
    }
}

pub fn cpu_seconds(pid: u32) -> Option<f64> {
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if h.is_null() {
            return None;
        }
        let mut c = FILETIME::default();
        let mut e = FILETIME::default();
        let mut k = FILETIME::default();
        let mut u = FILETIME::default();
        let ok = GetProcessTimes(h, &mut c, &mut e, &mut k, &mut u);
        CloseHandle(h);
        if ok == 0 {
            return None;
        }
        let kk = ((k.dwHighDateTime as u64) << 32) | k.dwLowDateTime as u64;
        let uu = ((u.dwHighDateTime as u64) << 32) | u.dwLowDateTime as u64;
        Some((kk + uu) as f64 / 1e7)
    }
}

pub fn kill_tree(pid: u32) -> bool {
    use std::os::windows::process::CommandExt;
    std::process::Command::new("taskkill")
        .args(["/PID", &pid.to_string(), "/T", "/F"])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

pub fn no_window() -> u32 {
    CREATE_NO_WINDOW
}

#[link(name = "shell32")]
extern "system" {
    fn ShellExecuteW(
        hwnd: *mut c_void,
        op: *const u16,
        file: *const u16,
        params: *const u16,
        dir: *const u16,
        show: i32,
    ) -> *mut c_void;
}

/// 用系统 shell 打开文件或目录。
///
/// 不能直接 `Command::new("explorer")` —— 那样启动的 explorer.exe 会因
/// DLL 初始化失败而弹 "应用程序无法正常启动(0xc0000142)"。
/// ShellExecuteW 走的是 shell 的既有实例，稳定得多。
pub fn shell_open(path: &str) -> Result<(), String> {
    shell_exec_impl(path, None)
}

/// 用系统 shell 启动一个程序并带参数（不会闪控制台窗口）
pub fn shell_exec(exe: &str, params: &str) -> Result<(), String> {
    shell_exec_impl(exe, Some(params))
}

fn shell_exec_impl(file: &str, params: Option<&str>) -> Result<(), String> {
    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }
    let file_w = wide(file);
    let op = wide("open");
    let params_w = params.map(wide);
    let r = unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            op.as_ptr(),
            file_w.as_ptr(),
            params_w
                .as_ref()
                .map(|v| v.as_ptr())
                .unwrap_or(std::ptr::null()),
            std::ptr::null(),
            1, // SW_SHOWNORMAL
        )
    };
    let code = r as isize;
    if code > 32 {
        Ok(())
    } else {
        Err(format!("无法打开：{}（ShellExecute 返回 {}）", file, code))
    }
}

pub fn exit_text(rc: i32) -> String {
    if rc == 0 {
        return "正常退出".into();
    }
    let u = rc as u32;
    let name = match u {
        0xC000_0005 => Some("访问冲突（段错误）"),
        0xC000_001D => Some("非法指令"),
        0xC000_00FD => Some("栈溢出"),
        0xC000_0374 => Some("堆损坏"),
        0xC000_0409 => Some("栈缓冲区溢出 / 快速失败"),
        0xC000_0602 => Some("快速失败保护"),
        0x8000_0003 => Some("断点"),
        _ => None,
    };
    match name {
        Some(n) => format!("{} (0x{:08X})", n, u),
        None => format!("错误码 0x{:08X}", u),
    }
}
