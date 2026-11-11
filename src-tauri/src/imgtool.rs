//! 图像批处理：格式转换 / 分辨率缩放 / 体积压缩。
//!
//! 全部走 image crate（只吃 CPU，不依赖 Blender）。默认输出到新目录，不覆盖原图。

use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use image::{DynamicImage, ImageEncoder, ImageFormat};
use serde::{Deserialize, Serialize};
use tauri::Emitter;

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase", default)]
pub struct ProcCfg {
    pub dirs: String,
    pub depth: u32,
    /// 只处理这些扩展名（不含点），空=常见图片
    pub exts: String,
    /// 输出目录；空则 <源目录>\_processed
    pub out_dir: String,
    /// 文件名后缀，如 "_web"
    pub suffix: String,
    /// png / jpg / tiff / bmp / keep
    pub format: String,
    /// 1-100，仅 jpg 有效
    pub quality: u8,
    /// >0 时按目标体积自动搜质量（仅 jpg）
    pub target_kb: u32,
    /// none / percent / long_edge / exact
    pub resize: String,
    pub percent: f64,
    pub long_edge: u32,
    pub width: u32,
    pub height: u32,
    /// lanczos3 / catmullrom / triangle / nearest
    pub filter: String,
    /// 只缩小，不放大
    pub only_shrink: bool,
    /// 宽高强制偶数（视频编码需要）
    pub even: bool,
    /// 透明图转 JPG 时垫底：white / black / skip
    pub alpha: String,
    pub threads: u32,
}

impl Default for ProcCfg {
    fn default() -> Self {
        ProcCfg {
            dirs: String::new(),
            depth: 0,
            exts: String::new(),
            out_dir: String::new(),
            suffix: String::new(),
            format: "jpg".into(),
            quality: 85,
            target_kb: 0,
            resize: "none".into(),
            percent: 100.0,
            long_edge: 1920,
            width: 1920,
            height: 1080,
            filter: "lanczos3".into(),
            only_shrink: true,
            even: true,
            alpha: "white".into(),
            threads: 0,
        }
    }
}

#[derive(Serialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct ProcProgress {
    pub running: bool,
    pub done: usize,
    pub total: usize,
    pub percent: f64,
    pub file: String,
    pub failed: usize,
    pub message: String,
}

#[derive(Serialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct ProcResult {
    pub done: usize,
    pub skipped: usize,
    pub failed: usize,
    pub in_bytes: u64,
    pub out_bytes: u64,
    pub elapsed_ms: u64,
}

const DEFAULT_EXTS: [&str; 8] = ["png", "jpg", "jpeg", "bmp", "tif", "tiff", "tga", "webp"];

fn ext_of(p: &Path) -> String {
    p.extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default()
}

fn output_format(fmt: &str) -> Option<ImageFormat> {
    match fmt {
        "png" => Some(ImageFormat::Png),
        "jpg" | "jpeg" => Some(ImageFormat::Jpeg),
        "tiff" | "tif" => Some(ImageFormat::Tiff),
        "bmp" => Some(ImageFormat::Bmp),
        _ => None,
    }
}

fn filter_of(name: &str) -> image::imageops::FilterType {
    match name {
        "nearest" => image::imageops::FilterType::Nearest,
        "triangle" => image::imageops::FilterType::Triangle,
        "catmullrom" => image::imageops::FilterType::CatmullRom,
        _ => image::imageops::FilterType::Lanczos3,
    }
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

pub fn collect(cfg: &ProcCfg) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for d in cfg.dirs.split(',').map(|s| s.trim()).filter(|s| !s.is_empty()) {
        walk(Path::new(d), 0, cfg.depth, &mut files);
    }
    let exts: Vec<String> = if cfg.exts.trim().is_empty() {
        DEFAULT_EXTS.iter().map(|s| s.to_string()).collect()
    } else {
        cfg.exts
            .split(',')
            .map(|s| s.trim().trim_start_matches('.').to_ascii_lowercase())
            .filter(|s| !s.is_empty())
            .collect()
    };
    files.retain(|p| exts.contains(&ext_of(p)));
    files.sort();
    files
}

fn target_size(img: &DynamicImage, cfg: &ProcCfg) -> (u32, u32) {
    let (w, h) = (img.width(), img.height());
    let (mut nw, mut nh) = match cfg.resize.as_str() {
        "percent" => {
            let f = (cfg.percent / 100.0).max(0.01);
            ((w as f64 * f).round() as u32, (h as f64 * f).round() as u32)
        }
        "long_edge" => {
            let edge = cfg.long_edge.max(1);
            let longest = w.max(h);
            if longest == 0 {
                (w, h)
            } else {
                let f = edge as f64 / longest as f64;
                ((w as f64 * f).round() as u32, (h as f64 * f).round() as u32)
            }
        }
        "exact" => (cfg.width.max(1), cfg.height.max(1)),
        _ => (w, h),
    };
    if cfg.only_shrink && cfg.resize != "exact" {
        if nw > w || nh > h {
            nw = w;
            nh = h;
        }
    }
    nw = nw.max(1);
    nh = nh.max(1);
    if cfg.even {
        nw -= nw % 2;
        nh -= nh % 2;
        nw = nw.max(2);
        nh = nh.max(2);
    }
    (nw, nh)
}

fn flatten_alpha(img: &DynamicImage, color: &str) -> DynamicImage {
    let bg = if color == "black" {
        image::Rgba([0u8, 0, 0, 255])
    } else {
        image::Rgba([255u8, 255, 255, 255])
    };
    let mut canvas = image::RgbaImage::from_pixel(img.width(), img.height(), bg);
    let rgba = img.to_rgba8();
    image::imageops::overlay(&mut canvas, &rgba, 0, 0);
    DynamicImage::ImageRgba8(canvas)
}

fn encode(img: &DynamicImage, fmt: ImageFormat, quality: u8) -> Option<Vec<u8>> {
    let mut buf = Cursor::new(Vec::new());
    match fmt {
        ImageFormat::Jpeg => {
            let rgb = img.to_rgb8();
            let mut enc = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buf, quality);
            enc.encode_image(&rgb).ok()?;
        }
        ImageFormat::Png => {
            // 默认压缩级别太松，PNG 输出会比原图还大，这里用最高压缩
            use image::codecs::png::{CompressionType, FilterType as PngFilter, PngEncoder};
            let enc = PngEncoder::new_with_quality(
                &mut buf,
                CompressionType::Best,
                PngFilter::Adaptive,
            );
            enc.write_image(
                img.as_bytes(),
                img.width(),
                img.height(),
                img.color().into(),
            )
            .ok()?;
        }
        _ => {
            img.write_to(&mut buf, fmt).ok()?;
        }
    }
    Some(buf.into_inner())
}

/// 按目标体积二分搜索质量
fn encode_to_target(img: &DynamicImage, target: u64) -> Vec<(u8, Vec<u8>)> {
    let mut best: Option<(u8, Vec<u8>)> = None;
    let mut lo: u8 = 20;
    let mut hi: u8 = 95;
    let mut tries = 0;
    while lo <= hi && tries < 7 {
        let mid = lo + (hi - lo) / 2;
        if let Some(data) = encode(img, ImageFormat::Jpeg, mid) {
            if (data.len() as u64) <= target {
                best = Some((mid, data));
                lo = mid + 1;
            } else {
                if mid == 0 {
                    break;
                }
                hi = mid - 1;
            }
        } else {
            break;
        }
        tries += 1;
    }
    match best {
        Some(v) => vec![v],
        None => encode(img, ImageFormat::Jpeg, 40).map(|d| (40, d)).into_iter().collect(),
    }
}

fn out_path_for(src: &Path, cfg: &ProcCfg, new_ext: &str) -> PathBuf {
    let dir = if cfg.out_dir.trim().is_empty() {
        src.parent()
            .map(|p| p.join("_processed"))
            .unwrap_or_else(|| PathBuf::from("_processed"))
    } else {
        PathBuf::from(cfg.out_dir.trim())
    };
    let stem = src
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "image".into());
    dir.join(format!("{}{}.{}", stem, cfg.suffix, new_ext))
}

fn process_one(src: &Path, cfg: &ProcCfg) -> Result<u64, String> {
    let in_size = std::fs::metadata(src).map(|m| m.len()).unwrap_or(0);
    let img = image::ImageReader::open(src)
        .map_err(|e| e.to_string())?
        .with_guessed_format()
        .map_err(|e| e.to_string())?
        .decode()
        .map_err(|e| e.to_string())?;

    let fmt = if cfg.format == "keep" {
        image::ImageReader::open(src)
            .ok()
            .and_then(|r| r.with_guessed_format().ok())
            .and_then(|r| r.format())
            .unwrap_or(ImageFormat::Png)
    } else {
        output_format(&cfg.format).ok_or_else(|| format!("不支持的输出格式：{}", cfg.format))?
    };

    let has_alpha = img.color().has_alpha();
    if matches!(fmt, ImageFormat::Jpeg) && has_alpha && cfg.alpha == "skip" {
        return Ok(0); // 调用方记为跳过
    }

    let (nw, nh) = target_size(&img, cfg);
    let mut out_img = if nw != img.width() || nh != img.height() {
        img.resize_exact(nw, nh, filter_of(&cfg.filter))
    } else {
        img
    };

    if matches!(fmt, ImageFormat::Jpeg) && out_img.color().has_alpha() {
        out_img = flatten_alpha(&out_img, &cfg.alpha);
    }

    let bytes = if matches!(fmt, ImageFormat::Jpeg) && cfg.target_kb > 0 {
        let target = cfg.target_kb as u64 * 1024;
        let list = encode_to_target(&out_img, target);
        list.into_iter().next().map(|(_, d)| d).unwrap_or_default()
    } else {
        encode(&out_img, fmt, cfg.quality)
            .ok_or_else(|| "编码失败".to_string())?
    };
    if bytes.is_empty() {
        return Err("编码结果为空".into());
    }

    let ext = fmt.extensions_str().first().copied().unwrap_or("png");
    let dst = out_path_for(src, cfg, ext);
    if let Some(d) = dst.parent() {
        std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
    }
    std::fs::write(&dst, &bytes).map_err(|e| e.to_string())?;
    let _ = in_size;
    Ok(bytes.len() as u64)
}

pub fn run(app: Option<tauri::AppHandle>, cfg: &ProcCfg) -> Result<ProcResult, String> {
    let files = collect(cfg);
    if files.is_empty() {
        return Err("没有找到可处理的图片".into());
    }
    let total = files.len();
    let threads = if cfg.threads == 0 {
        std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).min(16)
    } else {
        cfg.threads as usize
    };

    let started = std::time::Instant::now();
    let idx = Arc::new(AtomicUsize::new(0));
    let done = Arc::new(AtomicUsize::new(0));
    let failed = Arc::new(AtomicUsize::new(0));
    let skipped = Arc::new(AtomicUsize::new(0));
    let out_bytes = Arc::new(AtomicUsize::new(0));
    let cfg = Arc::new(cfg.clone());
    let files = Arc::new(files);

    let mut handles = Vec::new();
    for _ in 0..threads {
        let idx = idx.clone();
        let done = done.clone();
        let failed = failed.clone();
        let skipped = skipped.clone();
        let out_bytes = out_bytes.clone();
        let cfg = cfg.clone();
        let files = files.clone();
        let app = app.clone();
        handles.push(std::thread::spawn(move || loop {
            let i = idx.fetch_add(1, Ordering::Relaxed);
            if i >= files.len() {
                break;
            }
            let src = &files[i];
            match process_one(src, &cfg) {
                Ok(0) => {
                    skipped.fetch_add(1, Ordering::Relaxed);
                }
                Ok(n) => {
                    out_bytes.fetch_add(n as usize, Ordering::Relaxed);
                }
                Err(_) => {
                    failed.fetch_add(1, Ordering::Relaxed);
                }
            }
            let d = done.fetch_add(1, Ordering::Relaxed) + 1;
            if let Some(a) = &app {
                if d % 3 == 0 || d == total {
                    let _ = a.emit(
                        "rg://imgproc",
                        ProcProgress {
                            running: d < total,
                            done: d,
                            total,
                            percent: d as f64 / total as f64 * 100.0,
                            file: src.file_name().unwrap_or_default().to_string_lossy().to_string(),
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

    let in_bytes: u64 = files
        .iter()
        .filter_map(|p| std::fs::metadata(p).ok())
        .map(|m| m.len())
        .sum();

    let res = ProcResult {
        done: done.load(Ordering::Relaxed) - failed.load(Ordering::Relaxed) - skipped.load(Ordering::Relaxed),
        skipped: skipped.load(Ordering::Relaxed),
        failed: failed.load(Ordering::Relaxed),
        in_bytes,
        out_bytes: out_bytes.load(Ordering::Relaxed) as u64,
        elapsed_ms: started.elapsed().as_millis() as u64,
    };

    if let Some(a) = &app {
        let _ = a.emit(
            "rg://imgproc",
            ProcProgress {
                running: false,
                done: total,
                total,
                percent: 100.0,
                file: String::new(),
                failed: res.failed,
                message: format!(
                    "完成 {} 个，跳过 {}，失败 {}",
                    res.done, res.skipped, res.failed
                ),
            },
        );
    }
    Ok(res)
}
