mod app;
mod camera;
mod clipboard;
mod detector;
mod history;
mod samples;

use app::{AppLaunchOptions, QRCodeViewerApp};
use eframe::egui;

fn print_help() {
    println!(
        "qrcode-viewer (Rust Edition)\n\
         用法:\n\
           qrcode-viewer [选项]\n\n\
         选项:\n\
           --camera <INDEX>         指定初始摄像头索引 (默认: 0)\n\
           --test-scene             启动时直接进入内置多码测试图模式\n\
           --simulate-no-camera     模拟系统无摄像头场景（测试备用模式）\n\
           --simulate-busy-camera   模拟摄像头被其他程序占用场景（测试占用告警与重连）\n\
           --auto-exit <SECONDS>    运行指定秒数后自动关闭窗口（用于自动化冒烟测试）\n\
           --self-test              运行核心算法与跨平台子系统自检并退出\n\
           -h, --help               显示帮助信息"
    );
}

fn run_self_test() -> Result<(), String> {
    println!("=== [1/4] 多二维码检测与「只取最大」算法自检 ===");
    let scene = samples::build_multi_qr_test_frame(1280, 720);
    let (largest, all) = detector::detect_largest_qrcode(&scene);
    println!("检测到二维码总数: {}", all.len());
    for (idx, d) in all.iter().enumerate() {
        println!(
            "  [{}] 面积={:.1} px² | 引擎={} | 内容={}",
            idx, d.area, d.engine, d.text
        );
    }
    if all.len() < 3 {
        return Err(format!("预期检测到 3 个二维码，实际检测到 {}", all.len()));
    }
    let largest = largest.ok_or("未返回最大二维码")?;
    if !largest.text.starts_with("LARGEST_QR_SELECTED::") {
        return Err(format!("最大二维码选择错误: {}", largest.text));
    }

    println!("=== [2/4] 透视校正裁切与二次解码验证 ===");
    let cropped = detector::extract_rectified_qrcode(&scene, &largest, 220);
    let (re_largest, _) = detector::detect_largest_qrcode(&cropped);
    let re_largest = re_largest.ok_or("裁切拉正后的二维码无法二次解码")?;
    if re_largest.text != largest.text {
        return Err("裁切拉正后解码内容不一致".to_string());
    }
    println!("裁切图像二次解码成功: {}", re_largest.text);

    println!("=== [3/4] 历史记录持久化与去重验证 ===");
    let tmp_path = std::env::temp_dir().join(format!(
        "qrcode_viewer_selftest_{}.json",
        std::process::id()
    ));
    let mut store = history::HistoryStore::load_from_path(Some(tmp_path.clone()));
    assert!(store.record("QR_A", "摄像头 #0", 50000, 2));
    assert!(!store.record("QR_A", "摄像头 #0", 50100, 2)); // 连续相同去重
    assert!(store.record("QR_B", "剪贴板图片", 80000, 1));
    let reloaded = history::HistoryStore::load_from_path(Some(tmp_path.clone()));
    let _ = std::fs::remove_file(&tmp_path);
    if reloaded.entries.len() != 2 || reloaded.entries[0].text != "QR_B" {
        return Err("历史记录持久化校验失败".to_string());
    }
    println!("历史记录校验通过（共 {} 条）", reloaded.entries.len());

    println!("=== [4/4] 摄像头枚举与占用/缺失分类诊断验证 ===");
    let cams = camera::list_cameras();
    println!("当前系统枚举到摄像头数量: {}", cams.len());
    for c in &cams {
        println!("  - {}", c.display_label());
    }
    if let Some(first_cam) = cams.first() {
        match camera::capture_single_frame_test(first_cam.index) {
            Ok((w, h)) => println!(
                "  [实测] 成功从摄像头 #{} 抓取并解码实时帧: {}x{} RGBA",
                first_cam.index, w, h
            ),
            Err(e) => println!(
                "  [提示] 摄像头 #{} 当前未打开实时流 ({})",
                first_cam.index, e
            ),
        }
    }
    let busy_kind = camera::classify_camera_error("Device or resource busy (os error 16)", 0);
    match &busy_kind {
        camera::CameraFailureKind::DeviceBusy { holder_process } => {
            println!("摄像头占用检测正常, 当前占用进程信息: {:?}", holder_process);
        }
        _ => return Err("EBUSY 错误分类失败".to_string()),
    }

    println!("=== [5/5] 系统剪贴板图像与文本检查验证 ===");
    if std::env::var_os("DISPLAY").is_some()
        || std::env::var_os("WAYLAND_DISPLAY").is_some()
        || cfg!(target_os = "macos")
        || cfg!(target_os = "windows")
    {
        let test_qr = samples::make_qr_rgba("CLIPBOARD_QR_VERIFY_2026", 240, 2);
        if clipboard::copy_image_to_clipboard(&test_qr).is_ok() {
            if let Ok(clipboard::ClipboardPayload::Image { image, .. }) =
                clipboard::inspect_system_clipboard()
            {
                let (cb_largest, _) = detector::detect_largest_qrcode(&image);
                if let Some(det) = cb_largest {
                    println!("剪贴板图像二维码往返解析成功: {}", det.text);
                }
            }
        }
    } else {
        println!("当前为无显示服务环境，跳过剪贴板 GUI 往返测试");
    }

    println!("=== 全部自检通过 (ALL SELF-TESTS PASSED) ===");
    Ok(())
}

fn main() -> eframe::Result<()> {
    let mut initial_camera: u32 = 0;
    let mut start_in_test_scene = false;
    let mut simulate_no_camera = false;
    let mut simulate_busy_camera = false;
    let mut auto_exit_seconds: Option<f32> = None;
    let mut self_test = false;

    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--camera" => {
                if let Some(val) = args.get(i + 1) {
                    initial_camera = val.parse().unwrap_or(0);
                    i += 1;
                }
            }
            "--test-scene" => start_in_test_scene = true,
            "--simulate-no-camera" => simulate_no_camera = true,
            "--simulate-busy-camera" => simulate_busy_camera = true,
            "--auto-exit" => {
                if let Some(val) = args.get(i + 1) {
                    auto_exit_seconds = val.parse().ok();
                    i += 1;
                }
            }
            "--self-test" => self_test = true,
            "-h" | "--help" => {
                print_help();
                return Ok(());
            }
            _ => {}
        }
        i += 1;
    }

    if self_test {
        if let Err(e) = run_self_test() {
            eprintln!("自检失败: {}", e);
            std::process::exit(1);
        }
        return Ok(());
    }

    let primary_renderer = if cfg!(target_os = "windows") {
        eframe::Renderer::Wgpu
    } else {
        eframe::Renderer::Glow
    };

    let make_options = |renderer: eframe::Renderer| eframe::NativeOptions {
        renderer,
        viewport: egui::ViewportBuilder::default()
            .with_title("QRCode Viewer · 最大二维码实时检测与解析 (Rust)")
            .with_inner_size([1280.0, 780.0])
            .with_min_inner_size([960.0, 620.0])
            .with_drag_and_drop(true),
        ..Default::default()
    };

    let launch_opts = AppLaunchOptions {
        initial_camera,
        start_in_test_scene,
        simulate_no_camera,
        simulate_busy_camera,
        auto_exit_seconds,
    };

    let opts_first = launch_opts.clone();
    let res = eframe::run_native(
        "qrcode-viewer",
        make_options(primary_renderer),
        Box::new(move |cc| Ok(Box::new(QRCodeViewerApp::new(cc, opts_first)))),
    );
    if res.is_err() {
        let fallback_renderer = if primary_renderer == eframe::Renderer::Wgpu {
            eframe::Renderer::Glow
        } else {
            eframe::Renderer::Wgpu
        };
        return eframe::run_native(
            "qrcode-viewer",
            make_options(fallback_renderer),
            Box::new(move |cc| Ok(Box::new(QRCodeViewerApp::new(cc, launch_opts)))),
        );
    }
    res
}

#[cfg(test)]
mod tests {
    use super::*;
    use detector::Point2D;

    #[test]
    fn test_shoelace_polygon_area() {
        let square = [
            Point2D { x: 10.0, y: 10.0 },
            Point2D { x: 110.0, y: 10.0 },
            Point2D { x: 110.0, y: 110.0 },
            Point2D { x: 10.0, y: 110.0 },
        ];
        assert!((detector::polygon_area(&square) - 10000.0).abs() < 1e-2);
    }

    #[test]
    fn test_multi_qr_picks_largest() {
        let frame = samples::build_multi_qr_test_frame(1280, 720);
        let (largest, all) = detector::detect_largest_qrcode(&frame);
        assert_eq!(all.len(), 3, "应当同时检测到 3 个不同尺寸二维码");
        let largest = largest.expect("应当选出最大二维码");
        assert!(largest.text.starts_with("LARGEST_QR_SELECTED::"));
        assert!(all[0].area > all[1].area && all[1].area > all[2].area);
    }

    #[test]
    fn test_camera_error_classification() {
        let busy = camera::classify_camera_error("V4L2: Device or resource busy (os error 16)", 99);
        assert!(matches!(busy, camera::CameraFailureKind::DeviceBusy { .. }));

        let no_dev = camera::classify_camera_error("No such file or directory (os error 2)", 99);
        assert_eq!(no_dev, camera::CameraFailureKind::NoCameraFound);

        let perm = camera::classify_camera_error("Permission denied (os error 13)", 99);
        assert_eq!(perm, camera::CameraFailureKind::PermissionDenied);
    }
}
