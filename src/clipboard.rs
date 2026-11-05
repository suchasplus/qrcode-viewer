use image::RgbaImage;
use std::path::PathBuf;
#[cfg(target_os = "linux")]
use std::process::{Command, Stdio};

#[derive(Debug, Clone)]
pub enum ClipboardPayload {
    Image {
        image: RgbaImage,
        signature: u64,
    },
    Text {
        text: String,
        signature: u64,
    },
    Empty,
}

fn fnv1a_hash(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf29ce484222325;
    for &b in bytes {
        hash ^= b as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

/// 读取系统剪贴板（优先检查剪贴板中是否有图片，若无图片则读取文本）
/// 同时兼容 Windows / macOS / Linux (X11, Wayland, GTK/GNOME, KDE/KWin)
pub fn inspect_system_clipboard() -> Result<ClipboardPayload, String> {
    // 1. 尝试通过 arboard 读取剪贴板图像
    if let Ok(mut cb) = arboard::Clipboard::new() {
        if let Ok(img_data) = cb.get_image() {
            let w = img_data.width as u32;
            let h = img_data.height as u32;
            let raw = img_data.bytes.into_owned();
            if w > 0 && h > 0 && raw.len() == (w * h * 4) as usize {
                let sig = fnv1a_hash(&raw);
                if let Some(rgba) = RgbaImage::from_raw(w, h, raw) {
                    return Ok(ClipboardPayload::Image {
                        image: rgba,
                        signature: sig,
                    });
                }
            }
        }
    }

    // 2. Linux (GTK/Mutter 或 KDE/KWin Wayland/X11) 外部工具兜底读取剪贴板图片
    #[cfg(target_os = "linux")]
    if let Some(rgba) = read_linux_clipboard_image_fallback() {
        let sig = fnv1a_hash(rgba.as_raw());
        return Ok(ClipboardPayload::Image {
            image: rgba,
            signature: sig,
        });
    }

    // 3. 尝试通过 arboard 读取剪贴板文本
    if let Ok(mut cb) = arboard::Clipboard::new() {
        if let Ok(text) = cb.get_text() {
            let trimmed = text.trim().to_string();
            if !trimmed.is_empty() {
                let sig = fnv1a_hash(trimmed.as_bytes());
                return Ok(ClipboardPayload::Text {
                    text: trimmed,
                    signature: sig,
                });
            }
        }
    }

    // 4. Linux 剪贴板文本兜底
    #[cfg(target_os = "linux")]
    if let Some(text) = read_linux_clipboard_text_fallback() {
        let trimmed = text.trim().to_string();
        if !trimmed.is_empty() {
            let sig = fnv1a_hash(trimmed.as_bytes());
            return Ok(ClipboardPayload::Text {
                text: trimmed,
                signature: sig,
            });
        }
    }

    Ok(ClipboardPayload::Empty)
}

/// 将文本写入系统剪贴板
pub fn copy_text_to_clipboard(text: &str) -> Result<(), String> {
    if let Ok(mut cb) = arboard::Clipboard::new() {
        if cb.set_text(text.to_string()).is_ok() {
            return Ok(());
        }
    }

    #[cfg(target_os = "linux")]
    {
        if std::env::var_os("WAYLAND_DISPLAY").is_some() {
            if let Ok(mut child) = Command::new("wl-copy")
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
            {
                if let Some(mut stdin) = child.stdin.take() {
                    use std::io::Write;
                    let _ = stdin.write_all(text.as_bytes());
                }
                if let Ok(status) = child.wait() {
                    if status.success() {
                        return Ok(());
                    }
                }
            }
        }
        if std::env::var_os("DISPLAY").is_some() {
            if let Ok(mut child) = Command::new("xclip")
                .args(["-selection", "clipboard"])
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
            {
                if let Some(mut stdin) = child.stdin.take() {
                    use std::io::Write;
                    let _ = stdin.write_all(text.as_bytes());
                }
                if let Ok(status) = child.wait() {
                    if status.success() {
                        return Ok(());
                    }
                }
            }
        }
    }

    Err("无法写入系统剪贴板".to_string())
}

/// 将 RGBA 图像写入系统剪贴板（方便测试剪贴板图片识别或复制二维码图像）
pub fn copy_image_to_clipboard(img: &RgbaImage) -> Result<(), String> {
    let (w, h) = img.dimensions();
    if let Ok(mut cb) = arboard::Clipboard::new() {
        let data = arboard::ImageData {
            width: w as usize,
            height: h as usize,
            bytes: std::borrow::Cow::Borrowed(img.as_raw()),
        };
        if cb.set_image(data).is_ok() {
            return Ok(());
        }
    }

    #[cfg(target_os = "linux")]
    {
        let mut png_bytes = Vec::new();
        let mut cursor = std::io::Cursor::new(&mut png_bytes);
        if image::DynamicImage::ImageRgba8(img.clone())
            .write_to(&mut cursor, image::ImageFormat::Png)
            .is_ok()
        {
            if std::env::var_os("WAYLAND_DISPLAY").is_some() {
                if let Ok(mut child) = Command::new("wl-copy")
                    .args(["--type", "image/png"])
                    .stdin(Stdio::piped())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn()
                {
                    if let Some(mut stdin) = child.stdin.take() {
                        use std::io::Write;
                        let _ = stdin.write_all(&png_bytes);
                    }
                    if let Ok(status) = child.wait() {
                        if status.success() {
                            return Ok(());
                        }
                    }
                }
            }
        }
    }

    Err("无法将图像写入系统剪贴板".to_string())
}

#[cfg(target_os = "linux")]
fn read_linux_clipboard_image_fallback() -> Option<RgbaImage> {
    if std::env::var_os("WAYLAND_DISPLAY").is_some() {
        if let Ok(out) = Command::new("wl-paste")
            .args(["--type", "image/png"])
            .stderr(Stdio::null())
            .output()
        {
            if out.status.success() && !out.stdout.is_empty() {
                if let Ok(dyn_img) = image::load_from_memory(&out.stdout) {
                    return Some(dyn_img.to_rgba8());
                }
            }
        }
    }
    if std::env::var_os("DISPLAY").is_some() {
        if let Ok(out) = Command::new("xclip")
            .args(["-selection", "clipboard", "-t", "image/png", "-o"])
            .stderr(Stdio::null())
            .output()
        {
            if out.status.success() && !out.stdout.is_empty() {
                if let Ok(dyn_img) = image::load_from_memory(&out.stdout) {
                    return Some(dyn_img.to_rgba8());
                }
            }
        }
    }
    None
}

#[cfg(target_os = "linux")]
fn read_linux_clipboard_text_fallback() -> Option<String> {
    if std::env::var_os("WAYLAND_DISPLAY").is_some() {
        if let Ok(out) = Command::new("wl-paste")
            .args(["--no-newline", "--type", "text/plain"])
            .stderr(Stdio::null())
            .output()
        {
            if out.status.success() && !out.stdout.is_empty() {
                return String::from_utf8(out.stdout).ok();
            }
        }
    }
    if std::env::var_os("DISPLAY").is_some() {
        if let Ok(out) = Command::new("xclip")
            .args(["-selection", "clipboard", "-o"])
            .stderr(Stdio::null())
            .output()
        {
            if out.status.success() && !out.stdout.is_empty() {
                return String::from_utf8(out.stdout).ok();
            }
        }
    }
    None
}

/// 跨桌面环境（macOS / Windows / Linux GTK + KDE）打开本地二维码图片文件
pub fn pick_qr_image_file() -> Option<PathBuf> {
    // Linux 下若检测到 KDE 桌面且存在 kdialog，或 GTK 桌面且存在 zenity，可与 XDG Portal 协同工作
    if let Some(path) = rfd::FileDialog::new()
        .set_title("选择包含二维码的图片")
        .add_filter("图像文件", &["png", "jpg", "jpeg", "bmp", "webp"])
        .pick_file()
    {
        return Some(path);
    }

    #[cfg(target_os = "linux")]
    {
        let desktop = std::env::var("XDG_CURRENT_DESKTOP")
            .unwrap_or_default()
            .to_lowercase();
        if desktop.contains("kde") {
            if let Ok(out) = Command::new("kdialog")
                .args(["--getopenfilename", ".", "*.png *.jpg *.jpeg *.bmp *.webp"])
                .stderr(Stdio::null())
                .output()
            {
                if out.status.success() {
                    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
                    if !s.is_empty() {
                        return Some(PathBuf::from(s));
                    }
                }
            }
        }
        if let Ok(out) = Command::new("zenity")
            .args([
                "--file-selection",
                "--title=选择包含二维码的图片",
                "--file-filter=*.png *.jpg *.jpeg *.bmp *.webp",
            ])
            .stderr(Stdio::null())
            .output()
        {
            if out.status.success() {
                let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
                if !s.is_empty() {
                    return Some(PathBuf::from(s));
                }
            }
        }
    }

    None
}
