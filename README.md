# QRCode Viewer (Rust Edition)

跨平台桌面二维码实时检测与解析程序（支持 **Linux [Wayland / X11 / GTK / KDE]**、**macOS**、**Windows**）。

启动后自动打开系统默认摄像头，在独立后台线程实时检测画面中的所有二维码，通过**鞋带公式（Shoelace formula）**精确计算任意旋转角度四边形的真实像素面积，**仅选中并解析画面中面积最大的 1 个二维码**，在界面上实时展示其透视校正实拍图、标准重绘二维码、解析出的字符串及历史记录。

---

## 核心特性

1. **同框多码自动取最大**：
   - 主引擎 `rqrr` (Quirc 纯 Rust 实现) + 补充引擎 `rxing` (ZXing 纯 Rust 实现)，返回精确四角顶点坐标。
   - 使用鞋带公式计算任意旋转四边形面积，画面中同时出现多个二维码时，亮绿色粗框高亮并解析最大二维码，其余较小二维码以灰色边框标注忽略。
2. **零阻塞多线程流水线**：
   - 摄像头采集、MJPEG/YUYV 解码（纯 Rust `zune-jpeg`）、二维码检测与透视拉正裁切全部在独立后台线程完成，UI 主线程仅负责 GPU 纹理渲染，彻底杜绝窗口无响应（Not Responding）。
3. **系统剪贴板深度集成**：
   - **剪贴板图片识别**：复制任意截图或图片到系统剪贴板后，按 `Ctrl+V` 或点击「检查剪贴板」（或开启「自动监听剪贴板」），自动检测剪贴板图片中的最大二维码并解析。
   - **剪贴板文本转二维码**：若系统剪贴板为文本内容，检查剪贴板时自动生成清晰二维码预览并记录。
   - **结果一键复制 / 自动复制**：支持复制解析出的字符串或直接将重绘二维码图像写入系统剪贴板。
4. **识别历史记录**：
   - 自动持久化保存至本地数据目录（`~/.local/share/qrcode-viewer/history.json` / macOS `Application Support` / Windows `AppData\Local`）。
   - 支持连续相同内容自动去重刷新时间、点击历史项回看二维码与完整文本、单条复制/删除与一键清空。
5. **多摄像头热切换**：
   - 自动枚举可用视频捕获设备（Linux 下自动过滤 V4L2 元数据子节点），支持在顶部下拉框随时热切换摄像头或刷新设备列表。
6. **无摄像头 / 摄像头被占用智能兜底**：
   - 当系统无摄像头硬件，或摄像头被其他程序独占（`EBUSY`）时，界面自动展示告警诊断卡片（Linux 下自动通过 `/proc/*/fd/*` 定位并显示占用摄像头的进程名称与 PID）。
   - 自动切换至「无摄像头工作模式」：支持剪贴板识别、拖拽本地图片进入窗口解析、打开本地图片文件（兼容 XDG Desktop Portal / GTK `zenity` / KDE `kdialog`）、内置多码测试图，同时支持后台每 3 秒自动探测摄像头释放并恢复实时流。

---

## 编译与运行

```bash
# 编译 Release 版本
cargo build --release

# 直接启动（默认连接 0 号摄像头）
./target/release/qrcode-viewer

# 指定摄像头索引启动
./target/release/qrcode-viewer --camera 0

# 启动内置多二维码测试场景（验证 3 码同框只取最大码）
./target/release/qrcode-viewer --test-scene

# 模拟「无摄像头」或「摄像头被占用」兜底界面
./target/release/qrcode-viewer --simulate-no-camera
./target/release/qrcode-viewer --simulate-busy-camera

# 运行核心算法与跨平台子系统自检
./target/release/qrcode-viewer --self-test
```
