# msu2-slint

把 [Slint](https://slint.dev) 界面实时镜像到 MSU2 160×80 副屏。

Slint 通过 `MinimalSoftwareWindow` 软件渲染到 RGB565 framebuffer，程序每帧与上一帧做
16×16 tile 差分，把相邻脏 tile 合并成矩形，再用厂商的“填充字 + 差分”编码经 USB CDC
发给屏幕。屏幕上没有变化的像素永远不会上线。

实测：60 fps 连续刷新，平均 681 B/帧，约 41 KB/s，占全速 USB 带宽的 3.4%。

## 页面

四页循环，触摸焊盘短按翻页，也可以定时自动轮播：

| 页 | 内容 | 数据源 |
|---|---|---|
| 0 | 时钟 | `localtime_r` |
| 1 | CPU 使用率 + 1 分钟负载 | `/proc/stat`、`/proc/loadavg` |
| 2 | 内存占用 | `/proc/meminfo` |
| 3 | 网络上下行速率 | `/sys/class/net/*/statistics` |

系统指标每秒刷新一次，变化的部分才会上线。

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
| `--press-delta` | 150 | 相对基线下降多少判定为按下 |

## 实现要点

- **首帧**用 `display_frame` 发全屏，之后只发差分
- **断线重连**：读写失败后按 0.5s → 5s 退避重连，成功后重发全屏并重新校准触摸
- **自适应采样**：空闲时 15 Hz，按住或等待电平恢复时 30 Hz；采样同时充当保活
- **触摸识别**：强信号（下降 ≥ 300）单次采样触发，弱信号 2 次去抖；迟滞防止在阈值附近翻转
- **回弹防护**：松手后必须看到电平回到安静区间并保持 3 次，才允许下一次按下；5 秒超时兜底
- **误码过滤**：偏离基线超过 2000 的采样直接丢弃
- **超时保护**：单次写入 3 秒超时，固件卡死时直接报错
- **不补偿突发**：落后超过一帧就重新对齐，不会攒一波猛发

## 换成自己的界面

编辑 `ui/app.slint`。它就是一个普通的 Slint 组件，宽高固定 160×80，`build.rs` 会在编译期
把它生成为 `AppWindow`。页面数量由 `src/main.rs` 的 `PAGES` 和 `.slint` 里的 `dots`
数组共同决定。

数据采集在 `src/metrics.rs`，想加新的指标（磁盘、温度、天气）只要往 `Metrics`
里加字段，再在 `.slint` 里加一页。
