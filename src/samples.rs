use image::{Rgba, RgbaImage};
use qrcode::{EcLevel, QrCode};

/// 生成指定像素尺寸的 RGBA 标准二维码图像
pub fn make_qr_rgba(data: &str, target_size: u32, quiet_zone_modules: u32) -> RgbaImage {
    let target_size = target_size.max(32);
    let code = match QrCode::with_error_correction_level(data.as_bytes(), EcLevel::M) {
        Ok(c) => c,
        Err(_) => match QrCode::new(data.as_bytes()) {
            Ok(c) => c,
            Err(_) => return RgbaImage::from_pixel(target_size, target_size, Rgba([255, 255, 255, 255])),
        },
    };

    let modules = code.width() as u32;
    let total_modules = modules + quiet_zone_modules * 2;
    let mut img = RgbaImage::from_pixel(target_size, target_size, Rgba([255, 255, 255, 255]));

    for py in 0..target_size {
        let my = (py * total_modules) / target_size;
        for px in 0..target_size {
            let mx = (px * total_modules) / target_size;
            if mx >= quiet_zone_modules
                && mx < quiet_zone_modules + modules
                && my >= quiet_zone_modules
                && my < quiet_zone_modules + modules
            {
                let qx = (mx - quiet_zone_modules) as usize;
                let qy = (my - quiet_zone_modules) as usize;
                if code[(qx, qy)] == qrcode::Color::Dark {
                    img.put_pixel(px, py, Rgba([0, 0, 0, 255]));
                }
            }
        }
    }

    img
}

fn paste_rotated(
    canvas: &mut RgbaImage,
    patch: &RgbaImage,
    center_x: i32,
    center_y: i32,
    angle_deg: f32,
) {
    let (pw, ph) = patch.dimensions();
    let diag = ((pw as f32).hypot(ph as f32).ceil() as i32) + 4;
    let half = diag / 2;
    let rad = -angle_deg.to_radians();
    let cos_a = rad.cos();
    let sin_a = rad.sin();
    let pcx = pw as f32 * 0.5;
    let pcy = ph as f32 * 0.5;

    let (cw, ch) = canvas.dimensions();
    for dy in -half..=half {
        let cy = center_y + dy;
        if cy < 0 || cy >= ch as i32 {
            continue;
        }
        for dx in -half..=half {
            let cx = center_x + dx;
            if cx < 0 || cx >= cw as i32 {
                continue;
            }
            let sx = (dx as f32 * cos_a - dy as f32 * sin_a + pcx).round() as i32;
            let sy = (dx as f32 * sin_a + dy as f32 * cos_a + pcy).round() as i32;
            if sx >= 0 && sx < pw as i32 && sy >= 0 && sy < ph as i32 {
                let px = patch.get_pixel(sx as u32, sy as u32);
                canvas.put_pixel(cx as u32, cy as u32, *px);
            }
        }
    }
}

/// 构建包含 3 个不同尺寸和旋转角度二维码的测试画面（验证同框多码只解析面积最大的二维码）
pub fn build_multi_qr_test_frame(width: u32, height: u32) -> RgbaImage {
    let mut canvas = RgbaImage::from_pixel(width, height, Rgba([240, 240, 240, 255]));

    // 1. 小号二维码 (150x150, 旋转 8 度)
    let qr_small = make_qr_rgba("SMALL_QR::id=101::size=150", 150, 2);
    paste_rotated(&mut canvas, &qr_small, 210, 220, 8.0);

    // 2. 中号二维码 (220x220, 旋转 -10 度)
    let qr_medium = make_qr_rgba("MEDIUM_QR::id=202::size=220", 220, 2);
    paste_rotated(&mut canvas, &qr_medium, 1030, 450, -10.0);

    // 3. 最大二维码 (340x340, 旋转 5 度) —— 预期唯一选中项
    let qr_large = make_qr_rgba(
        "LARGEST_QR_SELECTED::https://bunnywin.cn/qrcode-viewer?engine=rust&size=340",
        340,
        2,
    );
    paste_rotated(&mut canvas, &qr_large, 610, 360, 5.0);

    canvas
}
