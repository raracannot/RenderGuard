//! 输出文件完整性校验：比"文件存在"可靠得多。
//!
//! 崩溃时 Blender 不会留下半成品帧（实测：正在渲染的帧根本没有文件），
//! 但断电、磁盘满、写出中途被杀仍可能留下截断文件，所以按魔数校验。

use std::fs::File;
use std::io::Read;
use std::path::Path;

fn ext_of(p: &Path) -> String {
    p.extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
}

pub fn verify(path: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    if meta.len() < 64 {
        return false;
    }
    let Ok(mut f) = File::open(path) else {
        return false;
    };
    let mut head = [0u8; 16];
    let n = f.read(&mut head).unwrap_or(0);
    if n < 4 {
        return false;
    }
    let e = ext_of(path);
    match e.as_str() {
        "png" => head[..8] == [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A],
        "jpg" | "jpeg" => head[..3] == [0xFF, 0xD8, 0xFF],
        "exr" => head[..4] == [0x76, 0x2F, 0x31, 0x01],
        "tif" | "tiff" => {
            head[..4] == [b'I', b'I', 0x2A, 0x00] || head[..4] == [b'M', b'M', 0x00, 0x2A]
        }
        "bmp" => head[..2] == [b'B', b'M'],
        "webp" => &head[..4] == b"RIFF" && n >= 12 && &head[8..12] == b"WEBP",
        "dpx" => &head[..4] == b"SDPX" || &head[..4] == b"XPDS",
        "hdr" => head[..2] == [b'#', b'?'],
        // TGA / Cineon / Iris / JPEG2000 无稳定魔数，只做长度检查
        _ => true,
    }
}
