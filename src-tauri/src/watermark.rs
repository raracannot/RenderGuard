//! 批量水印：文字（GDI 绘制，支持中文）或图像（PNG 带透明）。
//!
//! 独立工具，不并入转换管线。支持单点定位（九宫格 + 偏移）与平铺（自定义间距）。

#![allow(non_snake_case)]

use std::ffi::c_void;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use image::{DynamicImage, ImageFormat, Rgba, RgbaImage};
use serde::{Deserialize, Serialize};
use tauri::Emitter;

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase", default)]
pub struct WaterCfg {
    pub dirs: String,
    pub depth: u32,
    pub exts: String,
    pub out_dir: String,
    pub suffix: String,
    /// text 或 image
    pub kind: String,
    pub text: String,
    /// 字号：相对画面高度的百分比
    pub font_percent: f64,
    /// #RRGGBB
    pub color: String,
    pub opacity: f64,
    /// 图像水印路径
    pub image: String,
    /// 图像水印宽度：相对画面宽度的百分比
    pub image_percent: f64,
    /// single 或 tile
    pub mode: String,
    /// tl tc tr ml mc mr bl bc br
    pub pos: String,
    pub offset_x: i32,
    pub offset_y: i32,
    pub gap_x: u32,
    pub gap_y: u32,
    /// png / jpg，空则沿用原格式
    pub out_format: String,
    pub quality: u8,
    pub threads: u32,
}

impl Default for WaterCfg {
    fn default() -> Self {
        WaterCfg {
            dirs: String::new(),
            depth: 0,
            exts: String::new(),
            out_dir: String::new(),
            suffix: "_wm".into(),
            kind: "text".into(),
            text: "© RARA".into(),
            font_percent: 4.0,
            color: "#FFFFFF".into(),
            opacity: 0.55,
            image: String::new(),
            image_percent: 12.0,
            mode: "single".into(),
            pos: "br".into(),
            offset_x: 32,
            offset_y: 32,
            gap_x: 120,
            gap_y: 90,
            out_format: String::new(),
            quality: 92,
            threads: 0,
        }
    }
}

#[derive(Serialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct WaterProgress {
    pub running: bool,
    pub done: usize,
    pub total: usize,
    pub percent: f64,
    pub file: String,
    pub failed: usize,
    pub message: String,
}

// ---------------------------------------------------------------- GDI 文字

type Hdc = *mut c_void;
type Hbitmap = *mut c_void;
type Hgdiobj = *mut c_void;

#[repr(C)]
struct BITMAPINFOHEADER {
    biSize: u32,
    biWidth: i32,
    biHeight: i32,
    biPlanes: u16,
    biBitCount: u16,
    biCompression: u32,
    biSizeImage: u32,
    biXPelsPerMeter: i32,
    biYPelsPerMeter: i32,
    biClrUsed: u32,
    biClrImportant: u32,
}

#[repr(C)]
struct BITMAPINFO {
    bmiHeader: BITMAPINFOHEADER,
    bmiColors: [u32; 3],
}

#[repr(C)]
struct RECT {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

#[link(name = "gdi32")]
extern "system" {
    fn CreateCompatibleDC(hdc: Hdc) -> Hdc;
    fn CreateDIBSection(
        hdc: Hdc,
        bmi: *const BITMAPINFO,
        usage: u32,
        bits: *mut *mut c_void,
        section: *mut c_void,
        offset: u32,
    ) -> Hbitmap;
    fn SelectObject(hdc: Hdc, obj: Hgdiobj) -> Hgdiobj;
    fn DeleteObject(obj: Hgdiobj) -> i32;
    fn DeleteDC(hdc: Hdc) -> i32;
    fn CreateFontW(
        height: i32,
        width: i32,
        escapement: i32,
        orientation: i32,
        weight: i32,
        italic: u32,
        underline: u32,
        strikeout: u32,
        charset: u32,
        out_precision: u32,
        clip_precision: u32,
        quality: u32,
        pitch_family: u32,
        face: *const u16,
    ) -> Hgdiobj;
    fn SetBkMode(hdc: Hdc, mode: i32) -> i32;
    fn SetTextColor(hdc: Hdc, color: u32) -> u32;
    fn DrawTextW(hdc: Hdc, text: *const u16, len: i32, rect: *mut RECT, flags: u32) -> i32;
}

const TRANSPARENT_MODE: i32 = 1;
const DT_CALCRECT: u32 = 0x0400;
const DT_CENTER: u32 = 0x0001;
const DT_VCENTER: u32 = 0x0004;
const DT_SINGLELINE: u32 = 0x0020;
const DIB_RGB_COLORS: u32 = 0;

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn parse_color(hex: &str) -> (u8, u8, u8) {
    let s = hex.trim().trim_start_matches('#');
    if s.len() >= 6 {
        let r = u8::from_str_radix(&s[0..2], 16).unwrap_or(255);
        let g = u8::from_str_radix(&s[2..4], 16).unwrap_or(255);
        let b = u8::from_str_radix(&s[4..6], 16).unwrap_or(255);
        return (r, g, b);
    }
    (255, 255, 255)
}

/// 用 GDI 把文字画成带 alpha 的位图：白字黑底，取亮度当 alpha
fn render_text(text: &str, font_px: u32, color: (u8, u8, u8), face: &str) -> Option<RgbaImage> {
    if text.trim().is_empty() || font_px < 2 {
        return None;
    }
    let face_w = wide(face);
    let text_w: Vec<u16> = text.encode_utf16().collect();

    unsafe {
        let hdc = CreateCompatibleDC(std::ptr::null_mut());
        if hdc.is_null() {
            return None;
        }
        let font = CreateFontW(
            -(font_px as i32),
            0,
            0,
            0,
            600, // semibold
            0,
            0,
            0,
            1,   // DEFAULT_CHARSET
            0,
            0,
            5,   // CLEARTYPE_QUALITY
            0,
            face_w.as_ptr(),
        );
        if font.is_null() {
            DeleteDC(hdc);
            return None;
        }
        SelectObject(hdc, font);
        SetBkMode(hdc, TRANSPARENT_MODE);

        // 先量尺寸
        let mut rc = RECT {
            left: 0,
            top: 0,
            right: 0,
            bottom: 0,
        };
        DrawTextW(
            hdc,
            text_w.as_ptr(),
            text_w.len() as i32,
            &mut rc,
            DT_CALCRECT | DT_CENTER | DT_SINGLELINE,
        );
        let pad = (font_px / 4).max(2) as i32;
        let bw = (rc.right - rc.left).max(4) + pad * 2;
        let bh = (rc.bottom - rc.top).max(4) + pad * 2;

        let bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: bw,
                biHeight: -bh, // 负数 = 自上而下
                biPlanes: 1,
                biBitCount: 32,
                biCompression: 0,
                biSizeImage: 0,
                biXPelsPerMeter: 0,
                biYPelsPerMeter: 0,
                biClrUsed: 0,
                biClrImportant: 0,
            },
            bmiColors: [0; 3],
        };
        let mut bits: *mut c_void = std::ptr::null_mut();
        let dib = CreateDIBSection(
            hdc,
            &bmi,
            DIB_RGB_COLORS,
            &mut bits,
            std::ptr::null_mut(),
            0,
        );
        if dib.is_null() || bits.is_null() {
            DeleteObject(font);
            DeleteDC(hdc);
            return None;
        }
        SelectObject(hdc, dib);

        let white: u32 = 0x00FF_FFFF;
        SetTextColor(hdc, white);
        let mut rc2 = RECT {
            left: 0,
            top: 0,
            right: bw,
            bottom: bh,
        };
        DrawTextW(
            hdc,
            text_w.as_ptr(),
            text_w.len() as i32,
            &mut rc2,
            DT_CENTER | DT_VCENTER | DT_SINGLELINE,
        );

        // BGRA → alpha
        let src = std::slice::from_raw_parts(bits as *const u8, (bw * bh * 4) as usize);
        let mut out = RgbaImage::new(bw as u32, bh as u32);
        for y in 0..bh as u32 {
            for x in 0..bw as u32 {
                let i = ((y * bw as u32 + x) * 4) as usize;
                let a = src[i + 2].max(src[i + 1]).max(src[i]); // 白字，取最大通道
                out.put_pixel(x, y, Rgba([color.0, color.1, color.2, a]));
            }
        }

        DeleteObject(dib);
        DeleteObject(font);
        DeleteDC(hdc);
        Some(out)
    }
}

// ---------------------------------------------------------------- 合成

fn blend(base: &mut RgbaImage, over: &RgbaImage, x: i64, y: i64, opacity: f64) {
    let op = opacity.clamp(0.0, 1.0);
    for (ox, oy, px) in over.enumerate_pixels() {
        let dx = x + ox as i64;
        let dy = y + oy as i64;
        if dx < 0 || dy < 0 || dx >= base.width() as i64 || dy >= base.height() as i64 {
            continue;
        }
        let a = (px[3] as f64 / 255.0) * op;
        if a <= 0.001 {
            continue;
        }
        let b = base.get_pixel_mut(dx as u32, dy as u32);
        for c in 0..3 {
            b[c] = (px[c] as f64 * a + b[c] as f64 * (1.0 - a)).round().clamp(0.0, 255.0) as u8;
        }
        b[3] = 255;
    }
}

fn anchor_pos(pos: &str, cw: u32, ch: u32, mw: u32, mh: u32, ox: i32, oy: i32) -> (i64, i64) {
    let (cw, ch, mw_i, mh_i) = (cw as i64, ch as i64, mw as i64, mh as i64);
    let (ox, oy) = (ox as i64, oy as i64);
    match pos {
        "tl" => (ox, oy),
        "tc" => ((cw - mw_i) / 2 + ox, oy),
        "tr" => (cw - mw_i - ox, oy),
        "ml" => (ox, (ch - mh_i) / 2 + oy),
        "mc" => ((cw - mw_i) / 2 + ox, (ch - mh_i) / 2 + oy),
        "mr" => (cw - mw_i - ox, (ch - mh_i) / 2 + oy),
        "bl" => (ox, ch - mh_i - oy),
        "bc" => ((cw - mw_i) / 2 + ox, ch - mh_i - oy),
        _ => (cw - mw_i - ox, ch - mh_i - oy),
    }
}

fn build_mark(img: &DynamicImage, cfg: &WaterCfg) -> Option<RgbaImage> {
    if cfg.kind == "image" {
        let mark = image::ImageReader::open(&cfg.image)
            .ok()?
            .with_guessed_format()
            .ok()?
            .decode()
            .ok()?
            .to_rgba8();
        let target_w =
            ((img.width() as f64 * cfg.image_percent / 100.0).round() as u32).max(4);
        let ratio = target_w as f64 / mark.width() as f64;
        let target_h = ((mark.height() as f64 * ratio).round() as u32).max(2);
        Some(image::imageops::resize(
            &mark,
            target_w,
            target_h,
            image::imageops::FilterType::Lanczos3,
        ))
    } else {
        let font_px = ((img.height() as f64 * cfg.font_percent / 100.0).round() as u32).max(8);
        render_text(&cfg.text, font_px, parse_color(&cfg.color), "Microsoft YaHei")
    }
}

fn apply_one(src: &Path, cfg: &WaterCfg) -> Result<(), String> {
    let img = image::ImageReader::open(src)
        .map_err(|e| e.to_string())?
        .with_guessed_format()
        .map_err(|e| e.to_string())?
        .decode()
        .map_err(|e| e.to_string())?;

    let mut canvas = img.to_rgba8();
    let mark = build_mark(&img, cfg).ok_or_else(|| "水印生成失败".to_string())?;

    if cfg.mode == "tile" {
        // 整组居中对齐，避免边缘那一列被裁掉
        let step_x = (mark.width() + cfg.gap_x) as i64;
        let step_y = (mark.height() + cfg.gap_y) as i64;
        let cw = canvas.width() as i64;
        let ch = canvas.height() as i64;
        let cols = ((cw + cfg.gap_x as i64) / step_x).max(1);
        let rows = ((ch + cfg.gap_y as i64) / step_y).max(1);
        let total_w = cols * mark.width() as i64 + (cols - 1) * cfg.gap_x as i64;
        let total_h = rows * mark.height() as i64 + (rows - 1) * cfg.gap_y as i64;
        let start_x = (cw - total_w) / 2;
        let start_y = (ch - total_h) / 2;

        for r in 0..rows {
            for c in 0..cols {
                let x = start_x + c * step_x;
                let y = start_y + r * step_y;
                blend(&mut canvas, &mark, x, y, cfg.opacity);
            }
        }
    } else {
        let (x, y) = anchor_pos(
            &cfg.pos,
            canvas.width(),
            canvas.height(),
            mark.width(),
            mark.height(),
            cfg.offset_x,
            cfg.offset_y,
        );
        blend(&mut canvas, &mark, x, y, cfg.opacity);
    }

    let fmt = match cfg.out_format.as_str() {
        "jpg" | "jpeg" => ImageFormat::Jpeg,
        "png" => ImageFormat::Png,
        _ => match src
            .extension()
            .map(|e| e.to_string_lossy().to_ascii_lowercase())
            .unwrap_or_default()
            .as_str()
        {
            "jpg" | "jpeg" => ImageFormat::Jpeg,
            _ => ImageFormat::Png,
        },
    };

    let mut out = if fmt == ImageFormat::Jpeg {
        let rgb = DynamicImage::ImageRgba8(canvas).to_rgb8();
        let mut buf = Vec::new();
        let mut enc = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buf, cfg.quality);
        enc.encode_image(&rgb).map_err(|e| e.to_string())?;
        buf
    } else {
        let mut buf = Vec::new();
        let enc = image::codecs::png::PngEncoder::new_with_quality(
            &mut buf,
            image::codecs::png::CompressionType::Best,
            image::codecs::png::FilterType::Adaptive,
        );
        use image::ImageEncoder;
        enc.write_image(
            canvas.as_raw(),
            canvas.width(),
            canvas.height(),
            image::ExtendedColorType::Rgba8,
        )
        .map_err(|e| e.to_string())?;
        buf
    };

    let dir = if cfg.out_dir.trim().is_empty() {
        src.parent()
            .map(|p| p.join("_watermarked"))
            .unwrap_or_else(|| PathBuf::from("_watermarked"))
    } else {
        PathBuf::from(cfg.out_dir.trim())
    };
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let stem = src
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "image".into());
    let ext = if fmt == ImageFormat::Jpeg { "jpg" } else { "png" };
    let dst = dir.join(format!("{}{}.{}", stem, cfg.suffix, ext));
    out.shrink_to_fit();
    std::fs::write(&dst, &out).map_err(|e| e.to_string())?;
    Ok(())
}

fn walk(dir: &Path, depth: u32, max_depth: u32, out: &mut Vec<PathBuf>) {
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
        } else {
            out.push(p);
        }
    }
}

pub fn run(app: Option<tauri::AppHandle>, cfg: &WaterCfg) -> Result<(usize, usize), String> {
    let mut files = Vec::new();
    for d in cfg.dirs.split(',').map(|s| s.trim()).filter(|s| !s.is_empty()) {
        walk(Path::new(d), 0, cfg.depth, &mut files);
    }
    let exts: Vec<String> = if cfg.exts.trim().is_empty() {
        vec!["png".into(), "jpg".into(), "jpeg".into(), "bmp".into(), "tif".into(), "tiff".into(), "webp".into()]
    } else {
        cfg.exts.split(',').map(|s| s.trim().trim_start_matches('.').to_ascii_lowercase()).collect()
    };
    files.retain(|p| {
        p.extension()
            .map(|e| exts.contains(&e.to_string_lossy().to_ascii_lowercase()))
            .unwrap_or(false)
    });
    files.sort();
    if files.is_empty() {
        return Err("没有找到可处理的图片".into());
    }

    let total = files.len();
    let threads = if cfg.threads == 0 {
        std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).min(16)
    } else {
        cfg.threads as usize
    };
    let idx = Arc::new(AtomicUsize::new(0));
    let done = Arc::new(AtomicUsize::new(0));
    let failed = Arc::new(AtomicUsize::new(0));
    let files = Arc::new(files);
    let cfg = Arc::new(cfg.clone());

    let mut handles = Vec::new();
    for _ in 0..threads {
        let idx = idx.clone();
        let done = done.clone();
        let failed = failed.clone();
        let files = files.clone();
        let cfg = cfg.clone();
        let app = app.clone();
        handles.push(std::thread::spawn(move || loop {
            let i = idx.fetch_add(1, Ordering::Relaxed);
            if i >= files.len() {
                break;
            }
            if apply_one(&files[i], &cfg).is_err() {
                failed.fetch_add(1, Ordering::Relaxed);
            }
            let d = done.fetch_add(1, Ordering::Relaxed) + 1;
            if let Some(a) = &app {
                if d % 3 == 0 || d == total {
                    let _ = a.emit(
                        "rg://watermark",
                        WaterProgress {
                            running: d < total,
                            done: d,
                            total,
                            percent: d as f64 / total as f64 * 100.0,
                            file: files[i].file_name().unwrap_or_default().to_string_lossy().to_string(),
                            failed: failed.load(Ordering::Relaxed),
                            message: String::new(),
                        },
                    );
                }
            }
        }));
    }
    for h in handles {
        let _ = h.join();
    }

    let f = failed.load(Ordering::Relaxed);
    let d = done.load(Ordering::Relaxed);
    if let Some(a) = &app {
        let _ = a.emit(
            "rg://watermark",
            WaterProgress {
                running: false,
                done: total,
                total,
                percent: 100.0,
                file: String::new(),
                failed: f,
                message: format!("完成 {} 个，失败 {}", d - f, f),
            },
        );
    }
    Ok((d - f, f))
}

/// 只处理第一张图，返回 JPEG 的 data URL（供界面预览）
pub fn preview(cfg: &WaterCfg) -> Result<String, String> {
    let mut files = Vec::new();
    for d in cfg.dirs.split(',').map(|s| s.trim()).filter(|s| !s.is_empty()) {
        walk(Path::new(d), 0, cfg.depth, &mut files);
    }
    let exts: Vec<String> = if cfg.exts.trim().is_empty() {
        vec!["png".into(), "jpg".into(), "jpeg".into(), "bmp".into(), "tif".into(), "tiff".into()]
    } else {
        cfg.exts.split(',').map(|s| s.trim().trim_start_matches('.').to_ascii_lowercase()).collect()
    };
    files.retain(|p| {
        p.extension()
            .map(|e| exts.contains(&e.to_string_lossy().to_ascii_lowercase()))
            .unwrap_or(false)
    });
    files.sort();
    let first = files.first().ok_or_else(|| "没有找到图片".to_string())?;

    // 生成带水印的图，压到 960 宽以内做预览
    let img = image::ImageReader::open(first)
        .map_err(|e| e.to_string())?
        .with_guessed_format()
        .map_err(|e| e.to_string())?
        .decode()
        .map_err(|e| e.to_string())?;
    let mut canvas = img.to_rgba8();
    let mark = build_mark(&img, cfg).ok_or_else(|| "水印生成失败".to_string())?;
    if cfg.mode == "tile" {
        let step_x = (mark.width() + cfg.gap_x) as i64;
        let step_y = (mark.height() + cfg.gap_y) as i64;
        let cw = canvas.width() as i64;
        let ch = canvas.height() as i64;
        let cols = ((cw + cfg.gap_x as i64) / step_x).max(1);
        let rows = ((ch + cfg.gap_y as i64) / step_y).max(1);
        let total_w = cols * mark.width() as i64 + (cols - 1) * cfg.gap_x as i64;
        let total_h = rows * mark.height() as i64 + (rows - 1) * cfg.gap_y as i64;
        let sx = (cw - total_w) / 2;
        let sy = (ch - total_h) / 2;
        for r in 0..rows {
            for c in 0..cols {
                blend(&mut canvas, &mark, sx + c * step_x, sy + r * step_y, cfg.opacity);
            }
        }
    } else {
        let (x, y) = anchor_pos(&cfg.pos, canvas.width(), canvas.height(), mark.width(), mark.height(), cfg.offset_x, cfg.offset_y);
        blend(&mut canvas, &mark, x, y, cfg.opacity);
    }

    let small = if canvas.width() > 960 {
        let ratio = 960.0 / canvas.width() as f64;
        image::imageops::resize(
            &canvas,
            960,
            (canvas.height() as f64 * ratio).round() as u32,
            image::imageops::FilterType::Lanczos3,
        )
    } else {
        canvas
    };
    let rgb = DynamicImage::ImageRgba8(small).to_rgb8();
    let mut buf = Vec::new();
    let mut enc = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buf, 82);
    enc.encode_image(&rgb).map_err(|e| e.to_string())?;
    Ok(format!("data:image/jpeg;base64,{}", crate::rename::base64_encode_bytes(&buf)))
}
