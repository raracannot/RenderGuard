//! 图片/媒体元信息读取。
//!
//! 走 `image` crate（只读文件头，不解码像素），速度快、格式覆盖广，
//! 也为将来的图像工具箱留好底座。

use std::path::Path;

#[derive(Clone, Debug, Default)]
pub struct ImageInfo {
    pub width: u32,
    pub height: u32,
    /// 文件里报告的位深（每通道），读不到时按格式猜
    pub depth: u8,
    pub channels: u8,
    /// image crate 识别出的格式名（小写）
    pub format: String,
    /// 文件大小（字节）
    pub bytes: u64,
}

/// 读尺寸：只解析文件头，不解码像素
pub fn read_image_info(path: &Path) -> Option<ImageInfo> {
    let reader = image::ImageReader::open(path).ok()?;
    let reader = reader.with_guessed_format().ok()?;
    let format = reader.format()?;
    let bytes = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    let (width, height) = reader.into_dimensions().ok()?;
    let (depth, channels) = header_depth_channels(path, format);
    Some(ImageInfo {
        width,
        height,
        depth,
        channels,
        format: format.extensions_str().first().copied().unwrap_or("?").to_string(),
        bytes,
    })
}

/// 只有 PNG / TIFF 的头能低成本拿到确切位深，其余按格式惯例给默认值
fn header_depth_channels(path: &Path, format: image::ImageFormat) -> (u8, u8) {
    use image::ImageFormat::*;
    match format {
        Png => png_depth(path).map(|d| (d, 4)).unwrap_or((8, 4)),
        Tiff => (16, 3),
        OpenExr => (32, 4),
        Hdr => (32, 3),
        Gif => (8, 4),
        _ => (8, 3),
    }
}

fn png_depth(path: &Path) -> Option<u8> {
    use std::io::Read;
    let mut f = std::fs::File::open(path).ok()?;
    let mut head = [0u8; 26];
    f.read_exact(&mut head).ok()?;
    if &head[1..4] != b"PNG" {
        return None;
    }
    Some(head[24])
}

/// 目录里所有文件的大小合计（用于体积类替换符）
pub fn dir_size(dir: &Path) -> u64 {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return 0;
    };
    rd.flatten()
        .filter_map(|e| e.metadata().ok())
        .filter(|m| m.is_file())
        .map(|m| m.len())
        .sum()
}
