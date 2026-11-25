# msu2-slint

把 [Slint](https://slint.dev) 界面实时镜像到 MSU2 160×80 副屏。

Slint 通过 `MinimalSoftwareWindow` 软件渲染到 RGB565 framebuffer，程序每帧与上一帧做
16×16 tile 差分，把相邻脏 tile 合并成矩形，再用厂商的“填充字 + 差分”编码经 USB CDC
发给屏幕。屏幕上没有变化的像素永远不会上线。

背面触摸焊盘用于翻页：一次完整的按下-释放切换到下一个页面。

实测：60 fps 连续刷新，平均 681 B/帧，约 41 KB/s，占全速 USB 带宽的 3.4%。

## 运行

```bash
# 自动查找 VID:PID 1a86:fe0c，60 fps，直到 Ctrl-C
cargo run -q -p msu2-slint

# 跑 20 秒
cargo run -q -p msu2-slint -- --duration-ms 20000

# 面板出现拖影时降低帧率
cargo run -q -p msu2-slint -- --fps 30

# 触摸不灵敏时降低按下阈值
cargo run -q -p msu2-slint -- --press-delta 80
```

## 参数

| 参数 | 默认 | 说明 |
|---|---|---|
| `--port` | 自动 | 串口路径，如 `/dev/ttyACM0` |
| `--fps` | 60 | 目标帧率 |
| `--duration-ms` | 0 | 运行时长，0 表示直到 Ctrl-C |
| `--press-delta` | 125 | 相对基线下降多少判定为按下 |

## 实现要点

- **首帧**用 `display_frame` 发全屏，之后只发差分
- **自适应采样**：空闲时 15 Hz，按住或等待电平恢复时 30 Hz；采样同时充当保活
- **触摸防抖**：5 点中值滤波 + 按下/释放各 2 次去抖 + 迟滞 + 释放后 800 ms 锁定
- **电平恢复才重新武装**：松手后必须看到电平回到安静区间并保持 3 次，才接受下一次按下
- **超时保护**：单次写入 3 秒超时，固件卡死时直接报错
- **不补偿突发**：落后超过一帧就重新对齐，不会攒一波猛发

## 换成自己的界面

编辑 `ui/app.slint`。它就是一个普通的 Slint 组件，宽高固定 160×80，`build.rs` 会在编译期
把它生成为 `AppWindow`。`titles` / `subtitles` 两个数组决定页面的文字内容，`PAGES`
常量在 `src/main.rs` 里，两边保持一致即可。
