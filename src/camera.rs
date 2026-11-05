use crate::detector::{detect_largest_qrcode, extract_rectified_qrcode, QRDetection};
use image::RgbaImage;
use nokhwa::{
    pixel_format::RgbAFormat,
    utils::{
        ApiBackend, CameraFormat, CameraIndex, FrameFormat, RequestedFormat, RequestedFormatType,
        Resolution,
    },
    Camera,
};
use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, SyncSender},
        Arc,
    },
    thread,
    time::Duration,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CameraDeviceInfo {
    pub index: u32,
    pub name: String,
    pub description: String,
}

impl CameraDeviceInfo {
    pub fn display_label(&self) -> String {
        #[cfg(target_os = "linux")]
        {
            format!("/dev/video{} · {}", self.index, self.name)
        }
        #[cfg(not(target_os = "linux"))]
        {
            format!("#{} · {}", self.index, self.name)
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CameraFailureKind {
    NoCameraFound,
    DeviceBusy { holder_process: Option<String> },
    PermissionDenied,
    OpenFailed(String),
}

impl CameraFailureKind {
    pub fn title(&self) -> &'static str {
        match self {
            Self::NoCameraFound => "未检测到摄像头设备",
            Self::DeviceBusy { .. } => "摄像头设备正被占用",
            Self::PermissionDenied => "无摄像头访问权限",
            Self::OpenFailed(_) => "摄像头连接不可用",
        }
    }

    pub fn detail_message(&self, device_index: u32) -> String {
        match self {
            Self::NoCameraFound => {
                "当前系统未发现可用摄像头硬件。已自动进入「无摄像头工作模式」：\n\
                 • 可直接复制截图/二维码图片到剪贴板，点击「检查剪贴板」或开启自动监听解析；\n\
                 • 可直接将本地图片拖入本窗口，或点击「打开图片文件」进行多码取最大识别；\n\
                 • 插入 USB 摄像头后点击顶部「刷新/重连」即可恢复实时画面。"
                    .to_string()
            }
            Self::DeviceBusy { holder_process } => {
                let holder_tip = match holder_process {
                    Some(proc_info) => format!("检测到占用进程：{}", proc_info),
                    None => format!("设备节点 #{} 返回 EBUSY（资源忙）", device_index),
                };
                format!(
                    "{}\n\
                     当前摄像头已被其他应用独占打开，程序已自动启用备用输入模式：\n\
                     1. 在顶部下拉框切换至其他可用摄像头；\n\
                     2. 关闭占用摄像头的程序后，程序会自动重连（或点击「立即重连」）；\n\
                     3. 无需摄像头：直接使用「剪贴板识别」、「拖入图片文件」或「内置测试图」。",
                    holder_tip
                )
            }
            Self::PermissionDenied => {
                "系统拒绝了摄像头访问请求。\n\
                 • Linux：请确认当前用户在 `video` 组且拥有 `/dev/video*` 读写权限；\n\
                 • macOS / Windows：请在「系统设置 → 隐私与安全性 → 摄像头」中授权本程序。\n\
                 在此期间您仍可使用剪贴板识别与本地图片拖拽解析功能。"
                    .to_string()
            }
            Self::OpenFailed(raw) => {
                format!(
                    "打开摄像头 #{} 失败（{}）。\n\
                     提示：部分摄像头会注册多个虚拟/元数据节点（如 /dev/video1），请尝试切换至 #0 或使用剪贴板/图片模式。",
                    device_index, raw
                )
            }
        }
    }
}

/// 在 Linux 下检查 `/proc/*/fd/*` 找出当前占用 `/dev/videoN` 的进程名称与 PID
#[cfg(target_os = "linux")]
pub fn find_linux_video_holder(device_index: u32) -> Option<String> {
    let target = format!("/dev/video{}", device_index);
    let my_pid = std::process::id();
    let proc_dir = std::fs::read_dir("/proc").ok()?;

    for entry in proc_dir.flatten() {
        let fname = entry.file_name();
        let pid_str = fname.to_string_lossy();
        let Ok(pid) = pid_str.parse::<u32>() else {
            continue;
        };
        if pid == my_pid {
            continue;
        }
        let fd_dir_path = entry.path().join("fd");
        let Ok(fd_entries) = std::fs::read_dir(fd_dir_path) else {
            continue;
        };
        for fd in fd_entries.flatten() {
            if let Ok(link) = std::fs::read_link(fd.path()) {
                if link.to_string_lossy() == target {
                    let comm = std::fs::read_to_string(entry.path().join("comm"))
                        .unwrap_or_else(|_| "unknown".to_string())
                        .trim()
                        .to_string();
                    return Some(format!("{} (PID {})", comm, pid));
                }
            }
        }
    }
    None
}

#[cfg(not(target_os = "linux"))]
pub fn find_linux_video_holder(_device_index: u32) -> Option<String> {
    None
}

/// 枚举当前系统的摄像头列表
pub fn list_cameras() -> Vec<CameraDeviceInfo> {
    let backend = nokhwa::native_api_backend().unwrap_or(ApiBackend::Auto);
    let mut devices = Vec::new();

    if let Ok(infos) = nokhwa::query(backend) {
        for info in infos {
            let idx = match info.index() {
                CameraIndex::Index(i) => *i,
                CameraIndex::String(s) => s
                    .chars()
                    .filter(|c| c.is_ascii_digit())
                    .collect::<String>()
                    .parse::<u32>()
                    .unwrap_or(0),
            };
            devices.push(CameraDeviceInfo {
                index: idx,
                name: info.human_name(),
                description: info.description().to_string(),
            });
        }
    }

    // Linux 补充：若 nokhwa query 未返回完整列表，从 /sys/class/video4linux 扫描
    #[cfg(target_os = "linux")]
    if devices.is_empty() {
        if let Ok(entries) = std::fs::read_dir("/sys/class/video4linux") {
            for entry in entries.flatten() {
                let dir_name = entry.file_name().to_string_lossy().to_string();
                if let Some(num_str) = dir_name.strip_prefix("video") {
                    if let Ok(idx) = num_str.parse::<u32>() {
                        let dev_path = format!("/dev/video{}", idx);
                        if std::path::Path::new(&dev_path).exists() {
                            let name = std::fs::read_to_string(entry.path().join("name"))
                                .unwrap_or_else(|_| format!("Video Device {}", idx))
                                .trim()
                                .to_string();
                            devices.push(CameraDeviceInfo {
                                index: idx,
                                name,
                                description: dev_path,
                            });
                        }
                    }
                }
            }
        }
    }

    // Linux 过滤掉 V4L2 元数据子节点（如 /sys/class/video4linux/videoN/index != 0 的节点）
    #[cfg(target_os = "linux")]
    devices.retain(|d| {
        let sys_idx_path = format!("/sys/class/video4linux/video{}/index", d.index);
        if let Ok(raw) = std::fs::read_to_string(sys_idx_path) {
            raw.trim() == "0"
        } else {
            true
        }
    });

    devices.sort_by_key(|d| d.index);
    devices.dedup_by_key(|d| d.index);
    devices
}

pub fn classify_camera_error(err_msg: &str, device_index: u32) -> CameraFailureKind {
    let lower = err_msg.to_lowercase();
    if lower.contains("busy")
        || lower.contains("ebusy")
        || lower.contains("os error 16")
        || lower.contains("in use")
    {
        return CameraFailureKind::DeviceBusy {
            holder_process: find_linux_video_holder(device_index),
        };
    }
    if lower.contains("permission")
        || lower.contains("eacces")
        || lower.contains("os error 13")
        || lower.contains("access denied")
    {
        return CameraFailureKind::PermissionDenied;
    }
    if lower.contains("no such file")
        || lower.contains("not found")
        || lower.contains("os error 2")
        || lower.contains("os error 19")
    {
        return CameraFailureKind::NoCameraFound;
    }
    // 在 Linux 下，有时 V4L2 在设备忙时返回笼统的流打开错误，主动探测是否有进程占用该设备
    if let Some(holder) = find_linux_video_holder(device_index) {
        return CameraFailureKind::DeviceBusy {
            holder_process: Some(holder),
        };
    }
    CameraFailureKind::OpenFailed(err_msg.to_string())
}

pub struct FramePacket {
    pub frame: RgbaImage,
    pub largest: Option<QRDetection>,
    pub all_detections: Vec<QRDetection>,
    pub rectified_crop: Option<RgbaImage>,
}

pub enum CameraEvent {
    Connected {
        resolution: (u32, u32),
    },
    Frame(FramePacket),
    Error {
        failure: CameraFailureKind,
    },
}

pub struct CameraWorker {
    stop_flag: Arc<AtomicBool>,
    pub receiver: Receiver<CameraEvent>,
}

impl CameraWorker {
    pub fn spawn(device_index: u32, egui_ctx: eframe::egui::Context) -> Self {
        let stop_flag = Arc::new(AtomicBool::new(false));
        let stop_clone = Arc::clone(&stop_flag);
        // 容量为 1 的有界通道，确保 UI 线程永远只渲染最新一帧，零排队延迟
        let (tx, rx): (SyncSender<CameraEvent>, Receiver<CameraEvent>) = mpsc::sync_channel(1);

        thread::spawn(move || {
            Self::worker_loop(device_index, stop_clone, tx, egui_ctx);
        });

        Self {
            stop_flag,
            receiver: rx,
        }
    }

    fn worker_loop(
        device_index: u32,
        stop_flag: Arc<AtomicBool>,
        tx: SyncSender<CameraEvent>,
        egui_ctx: eframe::egui::Context,
    ) {
        let preferred = RequestedFormat::new::<RgbAFormat>(RequestedFormatType::Closest(
            CameraFormat::new(Resolution::new(1280, 720), FrameFormat::MJPEG, 30),
        ));
        let fallback =
            RequestedFormat::new::<RgbAFormat>(RequestedFormatType::AbsoluteHighestFrameRate);

        let mut camera = match Camera::new(CameraIndex::Index(device_index), preferred)
            .or_else(|_| Camera::new(CameraIndex::Index(device_index), fallback))
        {
            Ok(cam) => cam,
            Err(e) => {
                let failure = classify_camera_error(&e.to_string(), device_index);
                let _ = tx.send(CameraEvent::Error { failure });
                egui_ctx.request_repaint();
                return;
            }
        };

        if let Err(e) = camera.open_stream() {
            let failure = classify_camera_error(&e.to_string(), device_index);
            let _ = tx.send(CameraEvent::Error { failure });
            egui_ctx.request_repaint();
            return;
        }

        let res = camera.resolution();
        let _ = tx.send(CameraEvent::Connected {
            resolution: (res.width(), res.height()),
        });
        egui_ctx.request_repaint();

        let mut consecutive_errors = 0u32;
        while !stop_flag.load(Ordering::Relaxed) {
            match camera.frame() {
                Ok(buffer) => {
                    if stop_flag.load(Ordering::Relaxed) {
                        break;
                    }
                    consecutive_errors = 0;
                    if let Some(rgba) = decode_camera_buffer(&buffer) {
                        // 在后台工作线程内完成二维码检测与透视裁切，UI 主线程零阻塞
                        let (largest, all_detections) = detect_largest_qrcode(&rgba);
                        let rectified_crop = largest
                            .as_ref()
                            .map(|det| extract_rectified_qrcode(&rgba, det, 180));
                        let packet = FramePacket {
                            frame: rgba,
                            largest,
                            all_detections,
                            rectified_crop,
                        };
                        if tx.try_send(CameraEvent::Frame(packet)).is_ok() {
                            egui_ctx.request_repaint();
                        }
                    }
                }
                Err(e) => {
                    consecutive_errors += 1;
                    if consecutive_errors >= 8 {
                        let failure = classify_camera_error(&e.to_string(), device_index);
                        let _ = tx.send(CameraEvent::Error { failure });
                        egui_ctx.request_repaint();
                        break;
                    }
                    thread::sleep(Duration::from_millis(25));
                }
            }
        }
    }

    pub fn stop(&mut self) {
        self.stop_flag.store(true, Ordering::Relaxed);
    }
}

/// 用于自检：尝试从指定摄像头抓取并解码一帧真实画面
pub fn capture_single_frame_test(device_index: u32) -> Result<(u32, u32), String> {
    let preferred = RequestedFormat::new::<RgbAFormat>(RequestedFormatType::Closest(
        CameraFormat::new(Resolution::new(1280, 720), FrameFormat::MJPEG, 30),
    ));
    let fallback =
        RequestedFormat::new::<RgbAFormat>(RequestedFormatType::AbsoluteHighestFrameRate);

    let mut camera = Camera::new(CameraIndex::Index(device_index), preferred)
        .or_else(|_| Camera::new(CameraIndex::Index(device_index), fallback))
        .map_err(|e| e.to_string())?;
    camera.open_stream().map_err(|e| e.to_string())?;
    let buf = camera.frame().map_err(|e| e.to_string())?;
    let rgba = decode_camera_buffer(&buf).ok_or_else(|| "解码视频帧失败".to_string())?;
    Ok(rgba.dimensions())
}

fn decode_camera_buffer(buf: &nokhwa::Buffer) -> Option<RgbaImage> {
    if buf.source_frame_format() == FrameFormat::MJPEG {
        if let Ok(dyn_img) =
            image::load_from_memory_with_format(buf.buffer(), image::ImageFormat::Jpeg)
        {
            return Some(dyn_img.to_rgba8());
        }
    }
    if let Ok(rgba) = buf.decode_image::<RgbAFormat>() {
        return Some(rgba);
    }
    image::load_from_memory(buf.buffer())
        .ok()
        .map(|d| d.to_rgba8())
}
impl Drop for CameraWorker {
    fn drop(&mut self) {
        self.stop();
    }
}
