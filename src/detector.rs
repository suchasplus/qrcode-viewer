use image::{GrayImage, Rgba, RgbaImage};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Point2D {
    pub x: f32,
    pub y: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QRDetection {
    pub text: String,
    pub points: [Point2D; 4],
    pub area: f32,
    pub bbox: [u32; 4], // [x, y, w, h]
    pub engine: String,
}

/// 使用鞋带公式（Shoelace formula）计算任意四边形的真实像素面积
pub fn polygon_area(pts: &[Point2D; 4]) -> f32 {
    let mut sum = 0.0_f32;
    for i in 0..4 {
        let p1 = pts[i];
        let p2 = pts[(i + 1) % 4];
        sum += p1.x * p2.y - p2.x * p1.y;
    }
    sum.abs() * 0.5
}

fn compute_bbox(pts: &[Point2D; 4], width: u32, height: u32) -> [u32; 4] {
    let min_x = pts
        .iter()
        .map(|p| p.x.floor().max(0.0) as u32)
        .min()
        .unwrap_or(0)
        .min(width.saturating_sub(1));
    let min_y = pts
        .iter()
        .map(|p| p.y.floor().max(0.0) as u32)
        .min()
        .unwrap_or(0)
        .min(height.saturating_sub(1));
    let max_x = pts
        .iter()
        .map(|p| p.x.ceil().max(0.0) as u32)
        .max()
        .unwrap_or(1)
        .min(width);
    let max_y = pts
        .iter()
        .map(|p| p.y.ceil().max(0.0) as u32)
        .max()
        .unwrap_or(1)
        .min(height);

    [
        min_x,
        min_y,
        max_x.saturating_sub(min_x).max(1),
        max_y.saturating_sub(min_y).max(1),
    ]
}

fn rgba_to_luma(img: &RgbaImage) -> GrayImage {
    let (w, h) = img.dimensions();
    let mut luma = Vec::with_capacity((w * h) as usize);
    for px in img.pixels() {
        let [r, g, b, _] = px.0;
        // ITU-R BT.601 luma
        let y = ((77 * r as u32 + 150 * g as u32 + 29 * b as u32) >> 8) as u8;
        luma.push(y);
    }
    GrayImage::from_raw(w, h, luma).expect("valid luma dimensions")
}

/// 检测画面中的所有二维码，并按四边形面积从大到小排序返回
pub fn detect_all_qrcodes(img: &RgbaImage) -> Vec<QRDetection> {
    let (width, height) = img.dimensions();
    if width == 0 || height == 0 {
        return Vec::new();
    }

    let gray = rgba_to_luma(img);
    let mut detections: Vec<QRDetection> = Vec::new();
    let mut seen_texts: HashSet<String> = HashSet::new();

    // 1. 主引擎：rqrr (Quirc 纯 Rust 实现，天然返回精确四角顶点 bounds)
    let mut prepared = rqrr::PreparedImage::prepare(gray.clone());
    for grid in prepared.detect_grids() {
        if let Ok((_meta, text)) = grid.decode() {
            if text.is_empty() {
                continue;
            }
            let b = grid.bounds;
            let pts = [
                Point2D {
                    x: b[0].x as f32,
                    y: b[0].y as f32,
                },
                Point2D {
                    x: b[1].x as f32,
                    y: b[1].y as f32,
                },
                Point2D {
                    x: b[2].x as f32,
                    y: b[2].y as f32,
                },
                Point2D {
                    x: b[3].x as f32,
                    y: b[3].y as f32,
                },
            ];
            let area = polygon_area(&pts);
            let bbox = compute_bbox(&pts, width, height);
            seen_texts.insert(text.clone());
            detections.push(QRDetection {
                text,
                points: pts,
                area,
                bbox,
                engine: "rqrr".to_string(),
            });
        }
    }

    // 2. 补充引擎：仅当主引擎未检出时调用 rxing 兜底识别 MicroQR / 特殊畸变码
    if detections.is_empty() {
        let raw_luma = gray.into_raw();
        if let Ok(r) = rxing::helpers::detect_in_luma(
            raw_luma,
            width,
            height,
            Some(rxing::BarcodeFormat::QR_CODE),
        ) {
            for r in [r] {
            let fmt = r.getBarcodeFormat();
            if !matches!(
                fmt,
                rxing::BarcodeFormat::QR_CODE | rxing::BarcodeFormat::MICRO_QR_CODE
            ) {
                continue;
            }
            let text = r.getText().to_string();
            if text.is_empty() || seen_texts.contains(&text) {
                continue;
            }
            let rpts = r.getRXingResultPoints();
            let pts = if rpts.len() >= 4 {
                [
                    Point2D {
                        x: rpts[0].x,
                        y: rpts[0].y,
                    },
                    Point2D {
                        x: rpts[1].x,
                        y: rpts[1].y,
                    },
                    Point2D {
                        x: rpts[2].x,
                        y: rpts[2].y,
                    },
                    Point2D {
                        x: rpts[3].x,
                        y: rpts[3].y,
                    },
                ]
            } else if rpts.len() == 3 {
                // QR 三个定位点推算第四个角点 (bottom-left = [0], top-left = [1], top-right = [2])
                let bl = rpts[0];
                let tl = rpts[1];
                let tr = rpts[2];
                let br_x = tr.x + (bl.x - tl.x);
                let br_y = tr.y + (bl.y - tl.y);
                [
                    Point2D { x: tl.x, y: tl.y },
                    Point2D { x: tr.x, y: tr.y },
                    Point2D { x: br_x, y: br_y },
                    Point2D { x: bl.x, y: bl.y },
                ]
            } else {
                continue;
            };
            let area = polygon_area(&pts);
            let bbox = compute_bbox(&pts, width, height);
            seen_texts.insert(text.clone());
            detections.push(QRDetection {
                text,
                points: pts,
                area,
                bbox,
                engine: "rxing".to_string(),
            });
            }
        }
    }
    // 按真实面积降序排列，索引 0 即为画面中最大的二维码
    detections.sort_by(|a, b| {
        b.area
            .partial_cmp(&a.area)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    detections
}

/// 返回 (面积最大的二维码, 画面中检测到的全部二维码列表)
pub fn detect_largest_qrcode(img: &RgbaImage) -> (Option<QRDetection>, Vec<QRDetection>) {
    let all = detect_all_qrcodes(img);
    let largest = all.first().cloned();
    (largest, all)
}

/// 将检测到的最大二维码通过双线性四边形逆变换拉正并裁切为正方形图像
pub fn extract_rectified_qrcode(
    img: &RgbaImage,
    detection: &QRDetection,
    output_size: u32,
) -> RgbaImage {
    let mut out = RgbaImage::from_pixel(output_size, output_size, Rgba([255, 255, 255, 255]));
    let (src_w, src_h) = img.dimensions();
    if src_w == 0 || src_h == 0 || output_size == 0 {
        return out;
    }

    let [p0, p1, p2, p3] = detection.points;
    let margin = 0.08_f32; // 四周留出 8% 静区

    for y in 0..output_size {
        let v = (y as f32 / (output_size.saturating_sub(1).max(1) as f32) - margin)
            / (1.0 - 2.0 * margin);
        for x in 0..output_size {
            let u = (x as f32 / (output_size.saturating_sub(1).max(1) as f32) - margin)
                / (1.0 - 2.0 * margin);

            // 双线性插值映射 (u, v) -> 原图四边形内部或静区外延坐标
            let top_x = p0.x + u * (p1.x - p0.x);
            let top_y = p0.y + u * (p1.y - p0.y);
            let bot_x = p3.x + u * (p2.x - p3.x);
            let bot_y = p3.y + u * (p2.y - p3.y);

            let sx = (top_x + v * (bot_x - top_x)).round() as i32;
            let sy = (top_y + v * (bot_y - top_y)).round() as i32;

            if sx >= 0 && sx < src_w as i32 && sy >= 0 && sy < src_h as i32 {
                let px = img.get_pixel(sx as u32, sy as u32);
                out.put_pixel(x, y, *px);
            }
        }
    }

    out
}
