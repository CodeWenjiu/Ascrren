# msu2-slint

把 [Slint](https://slint.dev) 界面实时镜像到 MSU2 160×80 副屏。

Slint 通过 `MinimalSoftwareWindow` 软件渲染到 RGB565 framebuffer，程序每帧与上一帧做
16×16 tile 差分，把相邻脏 tile 合并成矩形，再用厂商的“填充字 + 差分”编码经 USB CDC
发给屏幕。屏幕上没有变化的像素永远不会上线。

实测：60 fps 连续刷新，平均 681 B/帧，约 41 KB/s，占全速 USB 带宽的 3.4%。

## 运行

```bash
# 自动查找 VID:PID 1a86:fe0c，60 fps，直到 Ctrl-C
cargo run -q -p msu2-slint

# 跑 20 秒
cargo run -q -p msu2-slint -- --duration-ms 20000

# 只渲染不发送，检查差分统计
cargo run -q -p msu2-slint -- --dry-run --verbose

# 面板出现拖影时降低帧率
cargo run -q -p msu2-slint -- --fps 30
```

## 参数

| 参数 | 默认 | 说明 |
|---|---|---|
| `--port` | 自动 | 串口路径，如 `/dev/ttyACM0` |
| `--fps` | 60 | 目标帧率 |
| `--duration-ms` | 0 | 运行时长，0 表示直到 Ctrl-C |
| `--dry-run` | 关 | 只渲染，不访问 USB |
| `--verbose` | 关 | 每帧打印差分统计 |
| `--dump-bmp` | 无 | 退出时把最后一帧写成 BMP |

## 实现要点

- **首帧**用 `display_frame` 发全屏，之后只发差分
- **保活**：连续 500 ms 无画面更新就发一次 ADC 读，否则固件回出场动画
- **超时保护**：单次写入 3 秒超时，固件卡死时直接报错
- **不补偿突发**：落后超过一帧就重新对齐，不会攒一波猛发

## 换成自己的界面

编辑 `ui/app.slint`。它就是一个普通的 Slint 组件，宽高固定 160×80，`build.rs` 会在编译期
把它生成为 `AppWindow`。数据绑定、动画、状态机都可以直接用，程序不需要知道 UI 长什么样。
