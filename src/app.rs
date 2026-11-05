use crate::{
    camera::{
        list_cameras, CameraDeviceInfo, CameraEvent, CameraFailureKind, CameraWorker, FramePacket,
    },
    clipboard::{
        copy_image_to_clipboard, copy_text_to_clipboard, inspect_system_clipboard,
        pick_qr_image_file, ClipboardPayload,
    },
    detector::{detect_largest_qrcode, extract_rectified_qrcode, QRDetection},
    history::HistoryStore,
    samples::{build_multi_qr_test_frame, make_qr_rgba},
};
use eframe::egui::{
    self, Color32, ColorImage, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Pos2,
    Rect, RichText, Sense, Stroke, StrokeKind, TextureHandle, TextureOptions, Vec2,
};
use image::RgbaImage;
use std::{
    path::PathBuf,
    time::{Duration, Instant},
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputSourceMode {
    LiveCamera,
    MultiQrTestScene,
    ClipboardImage,
    LocalImageFile(String),
}

#[derive(Debug, Clone)]
pub struct AppLaunchOptions {
    pub initial_camera: u32,
    pub start_in_test_scene: bool,
    pub simulate_no_camera: bool,
    pub simulate_busy_camera: bool,
    pub auto_exit_seconds: Option<f32>,
}

pub struct QRCodeViewerApp {
    // 设备与输入源状态
    cameras: Vec<CameraDeviceInfo>,
    selected_camera_idx: u32,
    source_mode: InputSourceMode,
    camera_worker: Option<CameraWorker>,
    camera_connected: bool,
    camera_resolution: Option<(u32, u32)>,
    camera_failure: Option<CameraFailureKind>,
    simulate_no_camera: bool,
    simulate_busy_camera: bool,
    auto_reconnect_camera: bool,
    last_reconnect_attempt: Instant,

    egui_ctx: egui::Context,

    // 图像与纹理
    current_frame: Option<RgbaImage>,
    test_scene_frame: Option<RgbaImage>,
    video_texture: Option<TextureHandle>,
    crop_qr_texture: Option<TextureHandle>,
    clean_qr_texture: Option<TextureHandle>,
    current_clean_qr_img: Option<RgbaImage>,

    // 检测与解码状态
    largest_detection: Option<QRDetection>,
    all_detections: Vec<QRDetection>,
    decoded_text: String,
    active_source_label: String,
    last_detected_time: String,
    status_toast: Option<(String, Instant)>,

    // 剪贴板交互设置
    auto_watch_clipboard: bool,
    auto_copy_decoded: bool,
    last_clipboard_sig: u64,
    last_clipboard_poll: Instant,

    // 历史记录
    history: HistoryStore,

    // 帧率统计与自动退出
    fps_ema: f32,
    last_frame_instant: Instant,
    started_at: Instant,
    auto_exit_seconds: Option<f32>,
}

impl QRCodeViewerApp {
    pub fn new(cc: &eframe::CreationContext<'_>, opts: AppLaunchOptions) -> Self {
        configure_cjk_fonts(&cc.egui_ctx);
        configure_sharp_theme(&cc.egui_ctx);

        let cameras = if opts.simulate_no_camera {
            Vec::new()
        } else {
            list_cameras()
        };

        let selected_camera_idx = if cameras.iter().any(|c| c.index == opts.initial_camera) {
            opts.initial_camera
        } else {
            cameras.first().map(|c| c.index).unwrap_or(opts.initial_camera)
        };

        let history = HistoryStore::load_or_default();

        if let Some(secs) = opts.auto_exit_seconds {
            let ctx_clone = cc.egui_ctx.clone();
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_secs_f32(secs.max(0.1)));
                ctx_clone.send_viewport_cmd(egui::ViewportCommand::Close);
                ctx_clone.request_repaint();
                std::thread::sleep(Duration::from_millis(250));
                std::process::exit(0);
            });
        }

        let mut app = Self {
            cameras,
            selected_camera_idx,
            source_mode: if opts.start_in_test_scene {
                InputSourceMode::MultiQrTestScene
            } else {
                InputSourceMode::LiveCamera
            },
            camera_worker: None,
            camera_connected: false,
            camera_resolution: None,
            camera_failure: None,
            simulate_no_camera: opts.simulate_no_camera,
            simulate_busy_camera: opts.simulate_busy_camera,
            auto_reconnect_camera: true,
            last_reconnect_attempt: Instant::now(),
            egui_ctx: cc.egui_ctx.clone(),
            current_frame: None,
            test_scene_frame: None,
            video_texture: None,
            crop_qr_texture: None,
            clean_qr_texture: None,
            current_clean_qr_img: None,
            largest_detection: None,
            all_detections: Vec::new(),
            decoded_text: String::new(),
            active_source_label: "-".to_string(),
            last_detected_time: "-".to_string(),
            status_toast: None,
            auto_watch_clipboard: false,
            auto_copy_decoded: false,
            last_clipboard_sig: 0,
            last_clipboard_poll: Instant::now(),
            history,
            fps_ema: 0.0,
            last_frame_instant: Instant::now(),
            started_at: Instant::now(),
            auto_exit_seconds: opts.auto_exit_seconds,
        };

        if app.source_mode == InputSourceMode::MultiQrTestScene {
            let frame = app.get_or_build_test_scene();
            app.process_frame(&cc.egui_ctx, frame, "内置多码测试图");
        } else {
            app.connect_selected_camera();
        }

        app
    }

    fn get_or_build_test_scene(&mut self) -> RgbaImage {
        if self.test_scene_frame.is_none() {
            self.test_scene_frame = Some(build_multi_qr_test_frame(1280, 720));
        }
        self.test_scene_frame.as_ref().unwrap().clone()
    }

    fn show_toast(&mut self, msg: impl Into<String>) {
        self.status_toast = Some((msg.into(), Instant::now()));
    }

    fn refresh_camera_list(&mut self) {
        if self.simulate_no_camera {
            self.cameras.clear();
        } else {
            self.cameras = list_cameras();
        }
        if !self.cameras.is_empty()
            && !self
                .cameras
                .iter()
                .any(|c| c.index == self.selected_camera_idx)
        {
            self.selected_camera_idx = self.cameras[0].index;
        }
    }

    fn connect_selected_camera(&mut self) {
        if let Some(mut worker) = self.camera_worker.take() {
            worker.stop();
        }
        self.camera_connected = false;
        self.camera_resolution = None;
        self.last_reconnect_attempt = Instant::now();

        if self.simulate_no_camera {
            self.camera_failure = Some(CameraFailureKind::NoCameraFound);
            return;
        }
        if self.simulate_busy_camera {
            self.camera_failure = Some(CameraFailureKind::DeviceBusy {
                holder_process: Some("simulated-holder (PID 99999)".to_string()),
            });
            return;
        }
        if self.cameras.is_empty() {
            self.refresh_camera_list();
            if self.cameras.is_empty() {
                self.camera_failure = Some(CameraFailureKind::NoCameraFound);
                return;
            }
        }

        self.camera_failure = None;
        self.camera_worker = Some(CameraWorker::spawn(
            self.selected_camera_idx,
            self.egui_ctx.clone(),
        ));
    }

    fn switch_to_camera(&mut self, index: u32) {
        self.selected_camera_idx = index;
        self.source_mode = InputSourceMode::LiveCamera;
        self.connect_selected_camera();
    }

    fn apply_frame_packet(&mut self, ctx: &egui::Context, packet: FramePacket, source_label: &str) {
        let now = Instant::now();
        let dt = now.duration_since(self.last_frame_instant).as_secs_f32();
        self.last_frame_instant = now;
        if dt > 0.0001 && dt < 2.0 {
            let inst_fps = 1.0 / dt;
            self.fps_ema = if self.fps_ema == 0.0 {
                inst_fps
            } else {
                self.fps_ema * 0.85 + inst_fps * 0.15
            };
        }

        let FramePacket {
            frame,
            largest,
            all_detections: all,
            rectified_crop,
        } = packet;

        if let Some(ref det) = largest {
            if let Some(ref rectified) = rectified_crop {
                self.crop_qr_texture = Some(ctx.load_texture(
                    "crop_qr_preview",
                    rgba_to_color_image(rectified),
                    TextureOptions::LINEAR,
                ));
            }

            let is_new_text = det.text != self.decoded_text;
            let area_u32 = det.area.round() as u32;
            let recorded_new = self
                .history
                .record(&det.text, source_label, area_u32, all.len());

            if is_new_text || recorded_new {
                self.decoded_text = det.text.clone();
                self.active_source_label = source_label.to_string();
                self.last_detected_time = chrono::Local::now().format("%H:%M:%S%.3f").to_string();

                let clean_qr = make_qr_rgba(&det.text, 180, 2);
                self.clean_qr_texture = Some(ctx.load_texture(
                    "clean_qr_preview",
                    rgba_to_color_image(&clean_qr),
                    TextureOptions::NEAREST,
                ));
                self.current_clean_qr_img = Some(clean_qr);

                println!(
                    "[{}] 识别到最大二维码 (来源={}, 同框共 {} 个, 面积={} px², 引擎={}): {}",
                    self.last_detected_time,
                    source_label,
                    all.len(),
                    area_u32,
                    det.engine,
                    det.text
                );

                if self.auto_copy_decoded {
                    if copy_text_to_clipboard(&det.text).is_ok() {
                        self.last_clipboard_sig = 0;
                        self.show_toast("已自动复制新识别内容到剪贴板");
                    }
                }
            }
        }

        self.video_texture = Some(ctx.load_texture(
            "main_video_frame",
            rgba_to_color_image(&frame),
            TextureOptions::LINEAR,
        ));
        self.current_frame = Some(frame);
        self.largest_detection = largest;
        self.all_detections = all;
    }

    fn process_frame(&mut self, ctx: &egui::Context, frame: RgbaImage, source_label: &str) {
        let (largest, all_detections) = detect_largest_qrcode(&frame);
        let rectified_crop = largest
            .as_ref()
            .map(|det| extract_rectified_qrcode(&frame, det, 180));
        self.apply_frame_packet(
            ctx,
            FramePacket {
                frame,
                largest,
                all_detections,
                rectified_crop,
            },
            source_label,
        );
    }

    fn handle_clipboard_check(&mut self, ctx: &egui::Context, manual_trigger: bool) {
        match inspect_system_clipboard() {
            Ok(ClipboardPayload::Image { image, signature }) => {
                if !manual_trigger && signature == self.last_clipboard_sig {
                    return;
                }
                self.last_clipboard_sig = signature;

                // 若处于实时摄像头连接状态，仅在手动点击或摄像头不可用时切换主视图
                if manual_trigger || self.camera_failure.is_some() {
                    if let Some(mut w) = self.camera_worker.take() {
                        w.stop();
                    }
                    self.source_mode = InputSourceMode::ClipboardImage;
                }

                let (largest, all) = detect_largest_qrcode(&image);
                let count = all.len();
                self.process_frame(ctx, image, "剪贴板图片");
                if let Some(det) = largest {
                    self.show_toast(format!(
                        "剪贴板图片识别成功（共 {} 个码，已选最大 {} px²）",
                        count,
                        det.area.round() as u32
                    ));
                } else if manual_trigger {
                    self.show_toast("剪贴板图片中未检测到二维码");
                }
            }
            Ok(ClipboardPayload::Text { text, signature }) => {
                if !manual_trigger && signature == self.last_clipboard_sig {
                    return;
                }
                self.last_clipboard_sig = signature;
                if !manual_trigger && text == self.decoded_text {
                    return;
                }

                let clean_qr = make_qr_rgba(&text, 180, 2);
                self.clean_qr_texture = Some(ctx.load_texture(
                    "clean_qr_preview",
                    rgba_to_color_image(&clean_qr),
                    TextureOptions::NEAREST,
                ));
                self.crop_qr_texture = Some(ctx.load_texture(
                    "crop_qr_preview",
                    rgba_to_color_image(&clean_qr),
                    TextureOptions::NEAREST,
                ));
                self.current_clean_qr_img = Some(clean_qr);
                self.decoded_text = text.clone();
                self.active_source_label = "剪贴板文本".to_string();
                self.last_detected_time = chrono::Local::now().format("%H:%M:%S").to_string();
                self.history.record(&text, "剪贴板文本", 0, 1);
                self.show_toast(format!("已读取剪贴板文本（{} 字符）并生成二维码", text.chars().count()));
            }
            Ok(ClipboardPayload::Empty) => {
                if manual_trigger {
                    self.show_toast("系统剪贴板为空（支持复制截图图片或文本）");
                }
            }
            Err(e) => {
                if manual_trigger {
                    self.show_toast(format!("读取剪贴板失败: {}", e));
                }
            }
        }
    }

    fn load_image_from_path(&mut self, ctx: &egui::Context, path: PathBuf) {
        match image::open(&path) {
            Ok(dyn_img) => {
                if let Some(mut w) = self.camera_worker.take() {
                    w.stop();
                }
                let rgba = dyn_img.to_rgba8();
                let fname = path
                    .file_name()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_else(|| path.display().to_string());
                self.source_mode = InputSourceMode::LocalImageFile(fname.clone());
                let label = format!("本地图片({})", fname);
                self.process_frame(ctx, rgba, &label);
                if self.largest_detection.is_some() {
                    self.show_toast(format!("已从图片 {} 解析出最大二维码", fname));
                } else {
                    self.show_toast(format!("图片 {} 中未检测到二维码", fname));
                }
            }
            Err(e) => {
                self.show_toast(format!("打开图片失败: {}", e));
            }
        }
    }
}

impl eframe::App for QRCodeViewerApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if let Some(limit) = self.auto_exit_seconds {
            if self.started_at.elapsed().as_secs_f32() >= limit {
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                return;
            }
        }

        // 1. 支持直接拖拽本地图片文件进窗口解析（当无摄像头或被占用时极方便）
        let dropped_files = ctx.input(|i| i.raw.dropped_files.clone());
        if let Some(first_file) = dropped_files.first() {
            if let Some(ref path) = first_file.path {
                self.load_image_from_path(ctx, path.clone());
            } else if let Some(ref bytes) = first_file.bytes {
                if let Ok(dyn_img) = image::load_from_memory(bytes) {
                    if let Some(mut w) = self.camera_worker.take() {
                        w.stop();
                    }
                    self.source_mode =
                        InputSourceMode::LocalImageFile(first_file.name.clone());
                    self.process_frame(ctx, dyn_img.to_rgba8(), "拖入图片");
                }
            }
        }

        // 2. 快捷键 Ctrl+V 立即检查并解析系统剪贴板
        let paste_pressed = ctx.input(|i| {
            i.modifiers.command && i.key_pressed(egui::Key::V)
                || i.events.iter().any(|ev| matches!(ev, egui::Event::Paste(_)))
        });
        if paste_pressed {
            self.handle_clipboard_check(ctx, true);
        }

        // 3. 自动监听剪贴板轮询
        if self.auto_watch_clipboard
            && self.last_clipboard_poll.elapsed() >= Duration::from_millis(800)
        {
            self.last_clipboard_poll = Instant::now();
            self.handle_clipboard_check(ctx, false);
        }

        // 4. 处理摄像头工作线程事件
        if self.source_mode == InputSourceMode::LiveCamera {
            let mut latest_frame = None;
            let mut cam_err = None;
            if let Some(ref worker) = self.camera_worker {
                while let Ok(ev) = worker.receiver.try_recv() {
                    match ev {
                        CameraEvent::Connected { resolution, .. } => {
                            self.camera_connected = true;
                            self.camera_resolution = Some(resolution);
                            self.camera_failure = None;
                        }
                        CameraEvent::Frame(packet) => {
                            self.camera_connected = true;
                            self.camera_failure = None;
                            latest_frame = Some(packet);
                        }
                        CameraEvent::Error { failure } => {
                            self.camera_connected = false;
                            cam_err = Some(failure);
                        }
                    }
                }
            }
            if let Some(failure) = cam_err {
                self.camera_failure = Some(failure);
                if let Some(mut w) = self.camera_worker.take() {
                    w.stop();
                }
            }
            if let Some(packet) = latest_frame {
                let src_label = format!("摄像头 #{}", self.selected_camera_idx);
                self.apply_frame_packet(ctx, packet, &src_label);
            }

            // 若摄像头处于占用/未连接异常状态，且开启了后台自动重连，每 3 秒自动探测一次
            if self.camera_failure.is_some()
                && self.auto_reconnect_camera
                && !self.simulate_no_camera
                && !self.simulate_busy_camera
                && self.last_reconnect_attempt.elapsed() >= Duration::from_secs(3)
            {
                self.connect_selected_camera();
            }
        }

        // 5. 渲染顶部工具栏（含摄像头切换、刷新、剪贴板检查、图片导入、多码测试图切换）
        egui::TopBottomPanel::top("top_header")
            .exact_height(48.0)
            .frame(
                egui::Frame::NONE
                    .fill(Color32::from_rgb(22, 22, 22))
                    .stroke(Stroke::new(1.0_f32, Color32::from_rgb(57, 57, 57)))
                    .inner_margin(egui::Margin::symmetric(14, 8)),
            )
            .show(ctx, |ui| {
                ui.horizontal_centered(|ui| {
                    ui.label(
                        RichText::new("QRCODE VIEWER // 跨平台摄像头最大二维码解析器")
                            .strong()
                            .size(14.0)
                            .color(Color32::from_rgb(244, 244, 244)),
                    );
                    ui.separator();

                    // 摄像头选择下拉框
                    ui.label(
                        RichText::new("摄像头:")
                            .size(12.0)
                            .color(Color32::from_rgb(168, 168, 168)),
                    );
                    let current_cam_label = if self.cameras.is_empty() {
                        "未检测到摄像头".to_string()
                    } else {
                        self.cameras
                            .iter()
                            .find(|c| c.index == self.selected_camera_idx)
                            .map(|c| c.display_label())
                            .unwrap_or_else(|| format!("设备 #{}", self.selected_camera_idx))
                    };

                    let mut target_switch_cam = None;
                    egui::ComboBox::from_id_salt("camera_selector_combo")
                        .selected_text(current_cam_label)
                        .width(220.0)
                        .show_ui(ui, |ui| {
                            if self.cameras.is_empty() {
                                ui.label("当前无可用摄像头设备");
                            } else {
                                for cam in &self.cameras {
                                    let is_selected = self.source_mode
                                        == InputSourceMode::LiveCamera
                                        && cam.index == self.selected_camera_idx;
                                    if ui
                                        .selectable_label(is_selected, cam.display_label())
                                        .clicked()
                                    {
                                        target_switch_cam = Some(cam.index);
                                    }
                                }
                            }
                        });
                    if let Some(new_idx) = target_switch_cam {
                        self.simulate_no_camera = false;
                        self.simulate_busy_camera = false;
                        self.switch_to_camera(new_idx);
                    }

                    if ui.button("刷新/重连").clicked() {
                        self.simulate_no_camera = false;
                        self.simulate_busy_camera = false;
                        self.refresh_camera_list();
                        self.source_mode = InputSourceMode::LiveCamera;
                        self.connect_selected_camera();
                    }

                    ui.separator();

                    // 剪贴板检查按钮 + 打开图片按钮 + 测试图切换
                    if ui
                        .button(
                            RichText::new("检查剪贴板 (Ctrl+V)")
                                .color(Color32::from_rgb(255, 255, 255)),
                        )
                        .clicked()
                    {
                        self.handle_clipboard_check(ctx, true);
                    }

                    if ui.button("打开图片").clicked() {
                        if let Some(path) = pick_qr_image_file() {
                            self.load_image_from_path(ctx, path);
                        }
                    }

                    let test_btn_label = if self.source_mode == InputSourceMode::MultiQrTestScene {
                        "返回实时摄像头"
                    } else {
                        "多码测试图"
                    };
                    if ui.button(test_btn_label).clicked() {
                        if self.source_mode == InputSourceMode::MultiQrTestScene {
                            self.source_mode = InputSourceMode::LiveCamera;
                            self.connect_selected_camera();
                        } else {
                            if let Some(mut w) = self.camera_worker.take() {
                                w.stop();
                            }
                            self.source_mode = InputSourceMode::MultiQrTestScene;
                            let frame = self.get_or_build_test_scene();
                            self.process_frame(ctx, frame, "内置多码测试图");
                        }
                    }

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let (badge_text, badge_bg) = match &self.source_mode {
                            InputSourceMode::LiveCamera => {
                                if self.camera_connected {
                                    (
                                        match self.camera_resolution {
                                            Some((w, h)) => format!(
                                                "实时摄像头 #{} ({}x{})",
                                                self.selected_camera_idx, w, h
                                            ),
                                            None => {
                                                format!("实时摄像头 #{}", self.selected_camera_idx)
                                            }
                                        },
                                        Color32::from_rgb(15, 98, 254),
                                    )
                                } else if self.camera_failure.is_some() {
                                    (
                                        "摄像头不可用 · 备用模式".to_string(),
                                        Color32::from_rgb(218, 30, 40),
                                    )
                                } else {
                                    (
                                        "正在连接摄像头...".to_string(),
                                        Color32::from_rgb(82, 82, 82),
                                    )
                                }
                            }
                            InputSourceMode::MultiQrTestScene => (
                                "模式：内置多码测试图".to_string(),
                                Color32::from_rgb(36, 161, 72),
                            ),
                            InputSourceMode::ClipboardImage => (
                                "模式：剪贴板图片".to_string(),
                                Color32::from_rgb(138, 63, 252),
                            ),
                            InputSourceMode::LocalImageFile(_) => (
                                "模式：本地图片".to_string(),
                                Color32::from_rgb(0, 114, 195),
                            ),
                        };

                        egui::Frame::NONE
                            .fill(badge_bg)
                            .inner_margin(egui::Margin::symmetric(10, 4))
                            .show(ui, |ui| {
                                ui.label(
                                    RichText::new(badge_text)
                                        .color(Color32::WHITE)
                                        .strong()
                                        .size(12.0),
                                );
                            });

                        ui.add_space(10.0);
                        ui.label(
                            RichText::new(format!("FPS: {:.1}", self.fps_ema))
                                .monospace()
                                .size(12.0)
                                .color(Color32::from_rgb(198, 198, 198)),
                        );
                    });
                });
            });

        // 6. 右侧检查器与历史记录面板
        egui::SidePanel::right("right_inspector_panel")
            .exact_width(430.0)
            .resizable(false)
            .frame(
                egui::Frame::NONE
                    .fill(Color32::from_rgb(38, 38, 38))
                    .stroke(Stroke::new(1.0_f32, Color32::from_rgb(57, 57, 57)))
                    .inner_margin(egui::Margin::same(14)),
            )
            .show(ctx, |ui| {
                ui.label(
                    RichText::new("最大二维码展示（实拍透视校正 vs 标准重绘）")
                        .size(11.0)
                        .strong()
                        .color(Color32::from_rgb(168, 168, 168)),
                );
                ui.add_space(6.0);

                ui.horizontal(|ui| {
                    // 实拍透视提取图
                    ui.vertical(|ui| {
                        ui.label(
                            RichText::new("画面最大码实拍提取")
                                .size(11.0)
                                .color(Color32::from_rgb(198, 198, 198)),
                        );
                        egui::Frame::NONE
                            .fill(Color32::from_rgb(22, 22, 22))
                            .stroke(Stroke::new(1.0_f32, Color32::from_rgb(82, 82, 82)))
                            .show(ui, |ui| {
                                ui.set_min_size(Vec2::splat(186.0));
                                ui.set_max_size(Vec2::splat(186.0));
                                if let Some(ref tex) = self.crop_qr_texture {
                                    ui.add(
                                        egui::Image::new(tex)
                                            .fit_to_exact_size(Vec2::splat(186.0)),
                                    );
                                } else {
                                    ui.centered_and_justified(|ui| {
                                        ui.label(
                                            RichText::new("等待检测")
                                                .color(Color32::from_rgb(111, 111, 111)),
                                        );
                                    });
                                }
                            });
                    });

                    ui.add_space(10.0);

                    // 标准清晰重绘二维码
                    ui.vertical(|ui| {
                        ui.label(
                            RichText::new("标准清晰重绘二维码")
                                .size(11.0)
                                .color(Color32::from_rgb(198, 198, 198)),
                        );
                        egui::Frame::NONE
                            .fill(Color32::from_rgb(22, 22, 22))
                            .stroke(Stroke::new(1.0_f32, Color32::from_rgb(82, 82, 82)))
                            .show(ui, |ui| {
                                ui.set_min_size(Vec2::splat(186.0));
                                ui.set_max_size(Vec2::splat(186.0));
                                if let Some(ref tex) = self.clean_qr_texture {
                                    ui.add(
                                        egui::Image::new(tex)
                                            .fit_to_exact_size(Vec2::splat(186.0)),
                                    );
                                } else {
                                    ui.centered_and_justified(|ui| {
                                        ui.label(
                                            RichText::new("等待检测")
                                                .color(Color32::from_rgb(111, 111, 111)),
                                        );
                                    });
                                }
                            });
                    });
                });

                ui.add_space(8.0);

                // 状态指标框
                egui::Frame::NONE
                    .fill(Color32::from_rgb(22, 22, 22))
                    .stroke(Stroke::new(1.0_f32, Color32::from_rgb(57, 57, 57)))
                    .inner_margin(egui::Margin::symmetric(10, 6))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        let area_str = self
                            .largest_detection
                            .as_ref()
                            .map(|d| format!("{:.0} px² (引擎: {})", d.area, d.engine))
                            .unwrap_or_else(|| "-".to_string());
                        ui.label(
                            RichText::new(format!(
                                "当前最大二维码面积: {}  |  来源: {}",
                                area_str, self.active_source_label
                            ))
                            .monospace()
                            .size(12.0),
                        );
                        ui.label(
                            RichText::new(format!("最近识别时间: {}", self.last_detected_time))
                                .size(11.0)
                                .color(Color32::from_rgb(168, 168, 168)),
                        );
                    });

                ui.add_space(8.0);

                // 剪贴板增强设置行
                ui.horizontal(|ui| {
                    ui.checkbox(&mut self.auto_watch_clipboard, "自动监听剪贴板");
                    ui.add_space(8.0);
                    ui.checkbox(&mut self.auto_copy_decoded, "识别后自动复制");
                });

                if let Some((ref msg, ts)) = self.status_toast {
                    if ts.elapsed() < Duration::from_secs(4) {
                        egui::Frame::NONE
                            .fill(Color32::from_rgb(15, 98, 254))
                            .inner_margin(egui::Margin::symmetric(8, 4))
                            .show(ui, |ui| {
                                ui.set_width(ui.available_width());
                                ui.label(RichText::new(msg).color(Color32::WHITE).size(11.5));
                            });
                    }
                }

                ui.add_space(6.0);

                // 解析字符串区域
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new("解析二维码字符串 (DECODED STRING)")
                            .size(11.0)
                            .strong()
                            .color(Color32::from_rgb(168, 168, 168)),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(
                            RichText::new(format!("{} 字符", self.decoded_text.chars().count()))
                                .size(11.0)
                                .color(Color32::from_rgb(168, 168, 168)),
                        );
                    });
                });

                egui::Frame::NONE
                    .fill(Color32::from_rgb(22, 22, 22))
                    .stroke(Stroke::new(1.0_f32, Color32::from_rgb(82, 82, 82)))
                    .inner_margin(egui::Margin::same(6))
                    .show(ui, |ui| {
                        egui::ScrollArea::vertical()
                            .id_salt("decoded_string_scroll")
                            .max_height(88.0)
                            .show(ui, |ui| {
                                let mut display_str = self.decoded_text.clone();
                                ui.add_sized(
                                    [ui.available_width(), 80.0],
                                    egui::TextEdit::multiline(&mut display_str)
                                        .font(FontId::monospace(13.5))
                                        .text_color(Color32::from_rgb(66, 190, 101))
                                        .frame(false)
                                        .hint_text("将二维码对准摄像头，或按 Ctrl+V 从剪贴板解析..."),
                                );
                            });
                    });

                ui.add_space(6.0);

                ui.horizontal(|ui| {
                    if ui
                        .button(
                            RichText::new("复制解析文本")
                                .color(Color32::WHITE)
                                .strong(),
                        )
                        .clicked()
                    {
                        if !self.decoded_text.is_empty() {
                            match copy_text_to_clipboard(&self.decoded_text) {
                                Ok(()) => self.show_toast("已复制解析文本到系统剪贴板"),
                                Err(e) => self.show_toast(e),
                            }
                        }
                    }

                    if ui.button("复制二维码图片").clicked() {
                        if let Some(ref img) = self.current_clean_qr_img {
                            let img_clone = img.clone();
                            match copy_image_to_clipboard(&img_clone) {
                                Ok(()) => self.show_toast("已将二维码图像写入系统剪贴板"),
                                Err(e) => self.show_toast(e),
                            }
                        }
                    }

                    if ui.button("检查剪贴板").clicked() {
                        self.handle_clipboard_check(ctx, true);
                    }

                    if ui.button("清空当前").clicked() {
                        self.decoded_text.clear();
                        self.crop_qr_texture = None;
                        self.clean_qr_texture = None;
                        self.current_clean_qr_img = None;
                    }
                });

                ui.add_space(10.0);
                ui.separator();
                ui.add_space(4.0);

                // 历史记录面板
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(format!(
                            "识别历史记录 ({} 条)",
                            self.history.entries.len()
                        ))
                        .size(11.0)
                        .strong()
                        .color(Color32::from_rgb(168, 168, 168)),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if !self.history.entries.is_empty() && ui.small_button("清空历史").clicked()
                        {
                            self.history.clear();
                            self.show_toast("已清空全部历史记录");
                        }
                    });
                });

                ui.add_space(4.0);

                let mut select_entry: Option<(String, String, String)> = None;
                let mut delete_id: Option<u64> = None;
                let mut copy_entry_text: Option<String> = None;

                egui::ScrollArea::vertical()
                    .id_salt("history_list_scroll")
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        if self.history.entries.is_empty() {
                            ui.add_space(16.0);
                            ui.vertical_centered(|ui| {
                                ui.label(
                                    RichText::new("暂无识别历史记录")
                                        .color(Color32::from_rgb(111, 111, 111)),
                                );
                            });
                        } else {
                            for entry in &self.history.entries {
                                let is_active = entry.text == self.decoded_text;
                                let border_col = if is_active {
                                    Color32::from_rgb(15, 98, 254)
                                } else {
                                    Color32::from_rgb(57, 57, 57)
                                };
                                egui::Frame::NONE
                                    .fill(Color32::from_rgb(22, 22, 22))
                                    .stroke(Stroke::new(1.0_f32, border_col))
                                    .inner_margin(egui::Margin::symmetric(8, 6))
                                    .show(ui, |ui| {
                                        ui.horizontal(|ui| {
                                            let meta = if entry.area_px > 0 {
                                                format!(
                                                    "#{} · {} · {} · {} px²",
                                                    entry.id,
                                                    entry.timestamp,
                                                    entry.source,
                                                    entry.area_px
                                                )
                                            } else {
                                                format!(
                                                    "#{} · {} · {}",
                                                    entry.id, entry.timestamp, entry.source
                                                )
                                            };
                                            ui.label(
                                                RichText::new(meta)
                                                    .size(10.5)
                                                    .color(Color32::from_rgb(168, 168, 168)),
                                            );
                                            ui.with_layout(
                                                egui::Layout::right_to_left(egui::Align::Center),
                                                |ui| {
                                                    if ui.small_button("×").clicked() {
                                                        delete_id = Some(entry.id);
                                                    }
                                                    if ui.small_button("复制").clicked() {
                                                        copy_entry_text = Some(entry.text.clone());
                                                    }
                                                    if ui.small_button("查看").clicked() {
                                                        select_entry = Some((
                                                            entry.text.clone(),
                                                            entry.source.clone(),
                                                            entry.timestamp.clone(),
                                                        ));
                                                    }
                                                },
                                            );
                                        });
                                        let preview: String =
                                            entry.text.chars().take(68).collect();
                                        let display_preview = if entry.text.chars().count() > 68 {
                                            format!("{}...", preview)
                                        } else {
                                            preview
                                        };
                                        ui.label(
                                            RichText::new(display_preview)
                                                .monospace()
                                                .size(12.0)
                                                .color(Color32::from_rgb(66, 190, 101)),
                                        );
                                    });
                                ui.add_space(4.0);
                            }
                        }
                    });

                if let Some(id) = delete_id {
                    self.history.remove_by_id(id);
                }
                if let Some(txt) = copy_entry_text {
                    if copy_text_to_clipboard(&txt).is_ok() {
                        self.show_toast("已复制历史记录内容到剪贴板");
                    }
                }
                if let Some((txt, src, ts)) = select_entry {
                    self.decoded_text = txt.clone();
                    self.active_source_label = format!("历史记录({})", src);
                    self.last_detected_time = ts;
                    let clean_qr = make_qr_rgba(&txt, 180, 2);
                    self.clean_qr_texture = Some(ctx.load_texture(
                        "clean_qr_preview",
                        rgba_to_color_image(&clean_qr),
                        TextureOptions::NEAREST,
                    ));
                    self.crop_qr_texture = Some(ctx.load_texture(
                        "crop_qr_preview",
                        rgba_to_color_image(&clean_qr),
                        TextureOptions::NEAREST,
                    ));
                    self.current_clean_qr_img = Some(clean_qr);
                }
            });

        // 7. 左侧主画面视口（实时画面 / 多码标注 / 摄像头占用或缺失兜底面板）
        egui::CentralPanel::default()
            .frame(
                egui::Frame::NONE
                    .fill(Color32::from_rgb(22, 22, 22))
                    .inner_margin(egui::Margin::same(12)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new("主画面视口（自动框选并解析面积最大的 1 个二维码，可拖入图片文件）")
                            .size(11.5)
                            .strong()
                            .color(Color32::from_rgb(168, 168, 168)),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let count = self.all_detections.len();
                        let count_label = if count > 1 {
                            format!(
                                "当前画面二维码数: {} (已选中最大 1 个，自动忽略其余 {} 个)",
                                count,
                                count - 1
                            )
                        } else {
                            format!("当前画面二维码数: {}", count)
                        };
                        ui.label(
                            RichText::new(count_label)
                                .monospace()
                                .strong()
                                .color(if count > 0 {
                                    Color32::from_rgb(66, 190, 101)
                                } else {
                                    Color32::from_rgb(198, 198, 198)
                                }),
                        );
                    });
                });

                ui.add_space(8.0);

                // 当处于摄像头模式且遇到「无摄像头」或「摄像头被占用」时，展示诊断与备用方案面板
                if self.source_mode == InputSourceMode::LiveCamera {
                    if let Some(failure) = self.camera_failure.clone() {
                        self.render_camera_fallback_card(ui, ctx, &failure);
                        ctx.request_repaint_after(Duration::from_millis(250));
                        return;
                    }
                }

                // 正常绘制视频/图像帧 + 最大二维码四边形叠加层
                let avail = ui.available_size();
                let (rect, _resp) = ui.allocate_exact_size(avail, Sense::hover());
                let painter = ui.painter_at(rect);
                painter.rect_filled(rect, CornerRadius::ZERO, Color32::from_rgb(11, 11, 11));
                painter.rect_stroke(
                    rect,
                    CornerRadius::ZERO,
                    Stroke::new(1.0_f32, Color32::from_rgb(57, 57, 57)),
                    StrokeKind::Inside,
                );

                if let (Some(ref tex), Some(ref frame_img)) =
                    (&self.video_texture, &self.current_frame)
                {
                    let (fw, fh) = frame_img.dimensions();
                    if fw > 0 && fh > 0 {
                        let scale = (rect.width() / fw as f32).min(rect.height() / fh as f32);
                        let draw_w = fw as f32 * scale;
                        let draw_h = fh as f32 * scale;
                        let img_rect =
                            Rect::from_center_size(rect.center(), Vec2::new(draw_w, draw_h));

                        painter.image(
                            tex.id(),
                            img_rect,
                            Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                            Color32::WHITE,
                        );

                        let map_pt = |p: crate::detector::Point2D| -> Pos2 {
                            Pos2::new(
                                img_rect.min.x + p.x * scale,
                                img_rect.min.y + p.y * scale,
                            )
                        };

                        // 先绘制被忽略的较小二维码（灰色边框）
                        for det in self.all_detections.iter().skip(1) {
                            let pts: Vec<Pos2> = det.points.iter().map(|&p| map_pt(p)).collect();
                            for i in 0..4 {
                                painter.line_segment(
                                    [pts[i], pts[(i + 1) % 4]],
                                    Stroke::new(2.0_f32, Color32::from_rgb(141, 141, 141)),
                                );
                            }
                            let top_left = Pos2::new(
                                img_rect.min.x + det.bbox[0] as f32 * scale,
                                (img_rect.min.y + det.bbox[1] as f32 * scale - 22.0)
                                    .max(img_rect.min.y),
                            );
                            let tag_rect =
                                Rect::from_min_size(top_left, Vec2::new(148.0, 20.0));
                            painter.rect_filled(
                                tag_rect,
                                CornerRadius::ZERO,
                                Color32::from_rgba_unmultiplied(22, 22, 22, 220),
                            );
                            painter.text(
                                top_left + Vec2::new(6.0, 3.0),
                                egui::Align2::LEFT_TOP,
                                format!("忽略 · {:.0} px²", det.area),
                                FontId::monospace(11.0),
                                Color32::from_rgb(198, 198, 198),
                            );
                        }

                        // 绘制选中的最大二维码（亮绿色粗框 + 蓝色顶点角标）
                        if let Some(ref largest) = self.largest_detection {
                            let pts: Vec<Pos2> =
                                largest.points.iter().map(|&p| map_pt(p)).collect();
                            for i in 0..4 {
                                painter.line_segment(
                                    [pts[i], pts[(i + 1) % 4]],
                                    Stroke::new(3.5_f32, Color32::from_rgb(66, 190, 101)),
                                );
                            }
                            for &corner in &pts {
                                let c_rect =
                                    Rect::from_center_size(corner, Vec2::splat(10.0));
                                painter.rect_filled(
                                    c_rect,
                                    CornerRadius::ZERO,
                                    Color32::from_rgb(15, 98, 254),
                                );
                                painter.rect_stroke(
                                    c_rect,
                                    CornerRadius::ZERO,
                                    Stroke::new(1.0_f32, Color32::WHITE),
                                    StrokeKind::Inside,
                                );
                            }

                            let top_left = Pos2::new(
                                img_rect.min.x + largest.bbox[0] as f32 * scale,
                                (img_rect.min.y + largest.bbox[1] as f32 * scale - 26.0)
                                    .max(img_rect.min.y),
                            );
                            let tag_rect =
                                Rect::from_min_size(top_left, Vec2::new(205.0, 24.0));
                            painter.rect_filled(
                                tag_rect,
                                CornerRadius::ZERO,
                                Color32::from_rgb(15, 98, 254),
                            );
                            painter.text(
                                top_left + Vec2::new(8.0, 4.0),
                                egui::Align2::LEFT_TOP,
                                format!("最大 QR · {:.0} px²", largest.area),
                                FontId::proportional(13.0),
                                Color32::WHITE,
                            );
                        }
                    }
                } else {
                    painter.text(
                        rect.center(),
                        egui::Align2::CENTER_CENTER,
                        "正在初始化视频流...",
                        FontId::proportional(15.0),
                        Color32::from_rgb(141, 141, 141),
                    );
                }
            });

        ctx.request_repaint_after(Duration::from_millis(30));
    }
}

impl QRCodeViewerApp {
    /// 当无摄像头或摄像头被其他程序占用时，渲染诊断信息与无缝备用工作台
    fn render_camera_fallback_card(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &egui::Context,
        failure: &CameraFailureKind,
    ) {
        egui::Frame::NONE
            .fill(Color32::from_rgb(30, 30, 30))
            .stroke(Stroke::new(1.0_f32, Color32::from_rgb(218, 30, 40)))
            .inner_margin(egui::Margin::same(24))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    egui::Frame::NONE
                        .fill(Color32::from_rgb(218, 30, 40))
                        .inner_margin(egui::Margin::symmetric(8, 4))
                        .show(ui, |ui| {
                            ui.label(
                                RichText::new("设备状态告警")
                                    .color(Color32::WHITE)
                                    .strong()
                                    .size(12.0),
                            );
                        });
                    ui.add_space(8.0);
                    ui.label(
                        RichText::new(failure.title())
                            .size(18.0)
                            .strong()
                            .color(Color32::from_rgb(255, 255, 255)),
                    );
                });

                ui.add_space(12.0);
                ui.label(
                    RichText::new(failure.detail_message(self.selected_camera_idx))
                        .size(13.5)
                        .color(Color32::from_rgb(224, 224, 224)),
                );

                ui.add_space(18.0);
                ui.separator();
                ui.add_space(12.0);

                ui.label(
                    RichText::new("可用操作与无摄像头工作模式：")
                        .strong()
                        .color(Color32::from_rgb(168, 168, 168)),
                );
                ui.add_space(8.0);

                ui.horizontal_wrapped(|ui| {
                    if ui
                        .button(
                            RichText::new("立即重试连接摄像头")
                                .color(Color32::WHITE)
                                .strong(),
                        )
                        .clicked()
                    {
                        self.simulate_no_camera = false;
                        self.simulate_busy_camera = false;
                        self.refresh_camera_list();
                        self.connect_selected_camera();
                    }

                    if ui.button("检查并解析系统剪贴板 (Ctrl+V)").clicked() {
                        self.handle_clipboard_check(ctx, true);
                    }

                    if ui.button("打开本地二维码图片文件").clicked() {
                        if let Some(path) = pick_qr_image_file() {
                            self.load_image_from_path(ctx, path);
                        }
                    }

                    if ui.button("进入内置多码测试图模式").clicked() {
                        self.source_mode = InputSourceMode::MultiQrTestScene;
                        let frame = self.get_or_build_test_scene();
                        self.process_frame(ctx, frame, "内置多码测试图");
                    }
                });

                ui.add_space(10.0);
                ui.checkbox(
                    &mut self.auto_reconnect_camera,
                    "后台每 3 秒自动探测摄像头是否释放/插入并自动恢复画面",
                );
            });
    }
}

fn rgba_to_color_image(img: &RgbaImage) -> ColorImage {
    let size = [img.width() as usize, img.height() as usize];
    ColorImage::from_rgba_unmultiplied(size, img.as_raw())
}

fn configure_sharp_theme(ctx: &egui::Context) {
    let mut style = (*ctx.style()).clone();
    style.visuals = egui::Visuals::dark();
    style.visuals.window_corner_radius = CornerRadius::ZERO;
    style.visuals.menu_corner_radius = CornerRadius::ZERO;
    style.visuals.widgets.noninteractive.corner_radius = CornerRadius::ZERO;
    style.visuals.widgets.inactive.corner_radius = CornerRadius::ZERO;
    style.visuals.widgets.hovered.corner_radius = CornerRadius::ZERO;
    style.visuals.widgets.active.corner_radius = CornerRadius::ZERO;
    style.visuals.widgets.open.corner_radius = CornerRadius::ZERO;
    style.visuals.panel_fill = Color32::from_rgb(22, 22, 22);
    style.visuals.selection.bg_fill = Color32::from_rgb(15, 98, 254);
    ctx.set_style(style);
}

fn configure_cjk_fonts(ctx: &egui::Context) {
    let candidate_paths = [
        // Linux (Arch / Omarchy / Ubuntu / Debian / Fedora / KDE / GNOME)
        "/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc",
        "/usr/share/fonts/noto-cjk/NotoSansCJK-Medium.ttc",
        "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
        "/usr/share/fonts/truetype/noto/NotoSansCJK-Regular.ttc",
        "/usr/share/fonts/google-noto-cjk/NotoSansCJK-Regular.ttc",
        "/usr/share/fonts/truetype/wqy/wqy-microhei.ttc",
        "/usr/share/fonts/wenquanyi/wqy-microhei/wqy-microhei.ttc",
        // macOS
        "/System/Library/Fonts/PingFang.ttc",
        "/System/Library/Fonts/STHeiti Light.ttc",
        "/System/Library/Fonts/Hiragino Sans GB.ttc",
        // Windows
        r"C:\Windows\Fonts\msyh.ttc",
        r"C:\Windows\Fonts\msyh.ttf",
        r"C:\Windows\Fonts\simhei.ttf",
    ];

    let mut loaded_bytes: Option<Vec<u8>> = None;
    for path in candidate_paths {
        if let Ok(bytes) = std::fs::read(path) {
            loaded_bytes = Some(bytes);
            break;
        }
    }

    #[cfg(target_os = "linux")]
    if loaded_bytes.is_none() {
        if let Ok(out) = std::process::Command::new("fc-match")
            .args(["-f", "%{file}", ":lang=zh"])
            .output()
        {
            if out.status.success() {
                let path_str = String::from_utf8_lossy(&out.stdout).trim().to_string();
                if !path_str.is_empty() {
                    if let Ok(bytes) = std::fs::read(path_str) {
                        loaded_bytes = Some(bytes);
                    }
                }
            }
        }
    }

    if let Some(bytes) = loaded_bytes {
        let mut fonts = FontDefinitions::default();
        fonts.font_data.insert(
            "cjk_system".to_string(),
            std::sync::Arc::new(FontData::from_owned(bytes)),
        );
        if let Some(fam) = fonts.families.get_mut(&FontFamily::Proportional) {
            fam.insert(0, "cjk_system".to_string());
        }
        if let Some(fam) = fonts.families.get_mut(&FontFamily::Monospace) {
            fam.push("cjk_system".to_string());
        }
        ctx.set_fonts(fonts);
    }
}
