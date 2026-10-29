use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum JobStatus {
    Queued,
    Running,
    Done,
    Partial,
    Failed,
    Stopped,
}

impl Default for JobStatus {
    fn default() -> Self {
        JobStatus::Queued
    }
}

/// 任务级参数覆盖。None 表示"沿用 .blend 里的原值"。
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct Overrides {
    pub camera: Option<String>,
    pub resolution: Option<[u32; 2]>,
    pub resolution_percentage: Option<u32>,
    pub fps: Option<f64>,
    pub frame_start: Option<i32>,
    pub frame_end: Option<i32>,
    pub frame_step: Option<i32>,
    pub format: Option<String>,
    pub output_dir: Option<String>,
    pub prefix: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct Progress {
    pub total: i64,
    pub done: i64,
    pub failed: i64,
    pub current: i64,
    pub percent: f64,
    pub restarts: u32,
    pub message: String,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase", default)]
pub struct Job {
    pub id: String,
    pub name: String,
    pub blender: String,
    pub blend: String,
    pub scene: String,
    pub engine: String,

    pub animation: bool,
    pub still_frame: i32,
    pub frame_start: i32,
    pub frame_end: i32,
    pub frame_step: i32,

    /// 逐帧输出路径的三段式描述（由探测脚本推导并校验过）
    pub path_prefix: String,
    pub path_pad: u32,
    pub path_suffix: String,
    pub template_ok: bool,
    pub is_movie: bool,

    pub overrides: Overrides,

    /// 启动时跳过已有帧（续渲染）；false 表示从头覆盖重渲
    pub resume: bool,
    /// 崩溃后自动重启
    pub auto_restart: bool,
    /// 同一帧最多重试次数
    pub max_retries: u32,
    /// 校验输出文件完整性
    pub verify: bool,

    pub status: JobStatus,
    pub progress: Progress,

    /// 探测时记录下来的 Cycles 设备偏好（纯净模式下用于还原，避免回落 CPU）
    #[serde(default)]
    pub cycles_device: String,
    #[serde(default)]
    pub cycles_devices: Vec<String>,

    #[serde(default)]
    pub attempts: BTreeMap<String, u32>,
    #[serde(default)]
    pub failed_frames: Vec<i32>,

    pub created: u64,
    pub elapsed: f64,
}

impl Default for Job {
    fn default() -> Self {
        Job {
            id: String::new(),
            name: String::new(),
            blender: String::new(),
            blend: String::new(),
            scene: String::new(),
            engine: String::new(),
            animation: true,
            still_frame: 1,
            frame_start: 1,
            frame_end: 1,
            frame_step: 1,
            path_prefix: String::new(),
            path_pad: 4,
            path_suffix: String::new(),
            template_ok: false,
            is_movie: false,
            overrides: Overrides::default(),
            resume: true,
            auto_restart: true,
            max_retries: 3,
            verify: true,
            status: JobStatus::Queued,
            progress: Progress::default(),
            cycles_device: String::new(),
            cycles_devices: Vec::new(),
            attempts: BTreeMap::new(),
            failed_frames: Vec::new(),
            created: 0,
            elapsed: 0.0,
        }
    }
}

impl Job {
    /// 需要渲染的帧列表（考虑 step 与静帧）
    pub fn frames(&self) -> Vec<i32> {
        if !self.animation {
            return vec![self.still_frame];
        }
        let step = self.frame_step.max(1);
        let mut out = Vec::new();
        let mut f = self.frame_start;
        while f <= self.frame_end {
            out.push(f);
            f += step;
        }
        out
    }

    /// 某一帧对应的输出文件路径；模板不可用时返回 None
    pub fn frame_path(&self, frame: i32) -> Option<PathBuf> {
        if !self.template_ok || self.path_prefix.is_empty() {
            return None;
        }
        let num = format!("{:0width$}", frame, width = self.path_pad.max(1) as usize);
        Some(PathBuf::from(format!(
            "{}{}{}",
            self.path_prefix, num, self.path_suffix
        )))
    }

    /// 该帧是否已经完成（存在且通过校验）
    pub fn is_done(&self, frame: i32) -> bool {
        let Some(p) = self.frame_path(frame) else {
            return false;
        };
        if !p.is_file() {
            return false;
        }
        if self.verify {
            crate::filecheck::verify(&p)
        } else {
            true
        }
    }

    #[allow(dead_code)]
    pub fn total_frames(&self) -> i64 {
        let step = if self.animation {
            self.frame_step.max(1) as i64
        } else {
            1
        };
        if !self.animation {
            return 1;
        }
        let span = (self.frame_end - self.frame_start) as i64;
        if span < 0 {
            0
        } else {
            span / step + 1
        }
    }

    fn count_done(&self, verify: bool) -> i64 {
        let frames = self.frames();
        let mut done = 0i64;
        for f in &frames {
            let hit = match self.frame_path(*f) {
                Some(p) if p.is_file() => {
                    if verify && self.verify {
                        crate::filecheck::verify(&p)
                    } else {
                        true
                    }
                }
                _ => false,
            };
            if hit {
                done += 1;
            }
        }
        done
    }

    /// 完整统计（含文件完整性校验），只在区段边界用
    pub fn recount(&mut self) {
        self.apply_count(self.count_done(true));
    }

    /// 轻量统计（只看文件是否存在），用于每秒进度刷新
    pub fn recount_quick(&mut self) {
        self.apply_count(self.count_done(false));
    }

    fn apply_count(&mut self, done: i64) {
        let total = self.frames().len() as i64;
        self.progress.total = total;
        self.progress.done = done;
        self.progress.failed = self.failed_frames.len() as i64;
        self.progress.percent = if total <= 0 {
            0.0
        } else {
            (done as f64) * 100.0 / (total as f64)
        };
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct SceneInfo {
    pub name: String,
    pub engine: String,
    pub frame_start: i32,
    pub frame_end: i32,
    pub frame_step: i32,
    pub cameras: Vec<String>,
    pub active_camera: Option<String>,
    pub resolution: [u32; 2],
    pub resolution_percentage: u32,
    pub fps: f64,
    pub filepath: String,
    pub format: String,
    pub color_mode: String,
    pub is_movie: bool,
    pub use_overwrite: bool,
    pub use_placeholder: bool,
    pub path_prefix: String,
    pub path_pad: u32,
    pub path_suffix: String,
    pub template_ok: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct ProbeResult {
    pub ok: bool,
    pub error: String,
    pub blender_version: String,
    pub blend_path: String,
    pub active_scene: String,
    pub scenes: Vec<SceneInfo>,
    pub cycles_device: String,
    pub cycles_devices: Vec<String>,
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct LogLine {
    pub level: String,
    pub text: String,
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct LogEvent {
    pub id: String,
    pub lines: Vec<LogLine>,
}
