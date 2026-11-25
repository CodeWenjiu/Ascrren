//! 长驻的 Slint → MSU2 实时显示桥。
//!
//! Slint 用 `MinimalSoftwareWindow` 软件渲染到 160×80 RGB565 framebuffer，
//! 每帧与上一帧做 tile 差分，只把变化的矩形通过 USB 发给屏幕。空闲时周期
//! 读 ADC 保活，避免固件回到出场动画。
//!
//! ```text
//! msu2-slint                  # 自动找设备，60 fps，直到 Ctrl-C
//! msu2-slint --dry-run        # 只渲染并打印差分统计
//! msu2-slint --fps 30         # 限制帧率
//! ```

use std::rc::Rc;
use std::time::{Duration, Instant};

use anyhow::{Context, anyhow};
use clap::Parser;
use msu2_core::display::{DISPLAY_HEIGHT, DISPLAY_WIDTH, Rect, TileGrid, rgb565_bytes};
use msu2_core::{ButtonConfig, ButtonState, Gesture, Msu2Device, STREAM_TIMEOUT};
use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType, Rgb565Pixel};
use slint::platform::{Platform, WindowAdapter};

slint::include_modules!();

const WIDTH: usize = DISPLAY_WIDTH as usize;
const HEIGHT: usize = DISPLAY_HEIGHT as usize;
const TILE_SIZE: u16 = 16;

/// 列表页的可选项。
const ITEMS: [&str; 3] = ["Clock", "System", "Settings"];

#[derive(Debug, Parser)]
#[command(name = "msu2-slint", about = "把 Slint 界面实时镜像到 MSU2 副屏")]
struct Cli {
    /// 串口路径；省略时自动查找 VID:PID 1a86:fe0c
    #[arg(long)]
    port: Option<String>,

    /// 串口波特率（USB CDC 下通常只是名义值）
    #[arg(long, default_value_t = 19200)]
    baud: u32,

    /// 握手/命令超时（毫秒）
    #[arg(long, default_value_t = 8000)]
    timeout_ms: u64,

    /// 目标帧率
    #[arg(long, default_value_t = 60)]
    fps: u64,

    /// 运行时长（毫秒）；0 表示一直运行到 Ctrl-C
    #[arg(long, default_value_t = 0)]
    duration_ms: u64,

    /// 只渲染不发送，用于检查 UI 和差分统计
    #[arg(long)]
    dry_run: bool,

    /// 每帧打印差分统计
    #[arg(long)]
    verbose: bool,

    /// 退出时把最后一帧写成 BMP
    #[arg(long, value_name = "FILE")]
    dump_bmp: Option<std::path::PathBuf>,

    /// 每 N 帧采样一次按键；60 fps 下默认为 30 Hz
    #[arg(long, default_value_t = 2)]
    sample_divisor: u64,

    /// 判定按下的下降幅度（相对基线）；灵敏度不够时调小
    #[arg(long, default_value_t = 125)]
    press_delta: u16,

    /// 中值滤波窗口；1 表示关闭
    #[arg(long, default_value_t = 3)]
    median_window: u8,

    /// 长按阈值（毫秒）
    #[arg(long, default_value_t = 600)]
    long_ms: u64,

    /// 超长按阈值（毫秒）
    #[arg(long, default_value_t = 1500)]
    very_long_ms: u64,

    /// 跟踪基线漂移（默认关闭；ADC 噪声会让基线随机游走）
    #[arg(long)]
    track_baseline: bool,

    /// 打印每一次按键采样
    #[arg(long)]
    trace: bool,
}

struct HeadlessPlatform {
    window: Rc<MinimalSoftwareWindow>,
    start: Instant,
}

impl Platform for HeadlessPlatform {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
        Ok(self.window.clone())
    }

    fn duration_since_start(&self) -> Duration {
        self.start.elapsed()
    }

    fn run_event_loop(&self) -> Result<(), slint::PlatformError> {
        Ok(())
    }
}

#[derive(Default)]
struct Stats {
    rendered: u64,
    sent: u64,
    dirty_tiles: u64,
    rects: u64,
    bytes: u64,
    samples: u64,
    sample_total: Duration,
    sample_max: Duration,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Page {
    List,
    Detail,
}

struct AppState {
    page: Page,
    focus: usize,
}

impl AppState {
    fn new() -> Self {
        Self {
            page: Page::List,
            focus: 0,
        }
    }

    fn apply(&mut self, gesture: Gesture) {
        match gesture {
            Gesture::Click => match self.page {
                Page::List => self.focus = (self.focus + 1) % ITEMS.len(),
                Page::Detail => self.page = Page::List,
            },
            Gesture::LongPress => {
                if self.page == Page::List {
                    self.page = Page::Detail;
                }
            }
            Gesture::VeryLongPress => self.page = Page::List,
        }
    }
}

fn refresh_ui(ui: &AppWindow, state: &AppState, button: &ButtonState, now: Instant) {
    ui.set_page(match state.page {
        Page::List => 0,
        Page::Detail => 1,
    });
    ui.set_selected(state.focus as i32);
    ui.set_detail_title(ITEMS[state.focus].into());
    ui.set_pressed(button.is_pressed());
    ui.set_hold_progress(button.hold_progress(now));
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    let window = MinimalSoftwareWindow::new(RepaintBufferType::ReusedBuffer);
    slint::platform::set_platform(Box::new(HeadlessPlatform {
        window: window.clone(),
        start: Instant::now(),
    }))
    .expect("Slint platform was already initialized");

    let ui = AppWindow::new().context("failed to build the Slint UI")?;
    window.set_size(slint::PhysicalSize::new(WIDTH as u32, HEIGHT as u32));
    ui.show().context("failed to show the Slint window")?;

    let mut device = if cli.dry_run {
        None
    } else {
        Some(connect(&cli).await?)
    };

    let grid = TileGrid::new(WIDTH as u16, HEIGHT as u16, TILE_SIZE)
        .context("failed to build the tile grid")?;
    let mut pixels = vec![Rgb565Pixel(0); WIDTH * HEIGHT];
    let mut current = vec![0u16; WIDTH * HEIGHT];
    let mut previous = vec![0u16; WIDTH * HEIGHT];

    // 先渲染并发送一整帧，后续只发差分。
    let mut state = AppState::new();
    let mut button = ButtonState::new(ButtonConfig {
        press_delta: cli.press_delta,
        release_delta: (u32::from(cli.press_delta) * 2 / 3) as u16,
        median_window: cli.median_window,
        long_press: Duration::from_millis(cli.long_ms),
        very_long_press: Duration::from_millis(cli.very_long_ms),
        track_baseline: cli.track_baseline,
        ..ButtonConfig::default()
    });

    slint::platform::update_timers_and_animations();
    refresh_ui(&ui, &state, &button, Instant::now());
    window.draw_if_needed(|renderer| {
        renderer.render(&mut pixels, WIDTH);
    });
    copy_pixels(&pixels, &mut current);

    let mut stats = Stats::default();
    if let Some(device) = device.as_mut() {
        let bytes = send_full_frame(device, &current).await?;
        stats.bytes += bytes as u64;
        stats.sent += 1;
        println!("initial frame sent ({bytes} B)");
    } else {
        println!("dry-run: initial frame rendered");
    }
    previous.copy_from_slice(&current);
    stats.rendered += 1;

    let interval = Duration::from_micros(1_000_000 / cli.fps.max(1));
    let started = Instant::now();
    let deadline = (cli.duration_ms > 0).then(|| started + Duration::from_millis(cli.duration_ms));
    let mut next_frame = Instant::now() + interval;
    let mut frame_index = 1u64;
    let mut calibration_announced = false;

    if device.is_some() {
        println!("calibrating the touch pad; keep hands off the back of the screen");
    }

    loop {
        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            break;
        }

        slint::platform::update_timers_and_animations();

        // 按键采样就是保活：每次 read_adc 都是一次完整的 USB 往返。
        if let Some(device) = device.as_mut()
            && frame_index.is_multiple_of(cli.sample_divisor.max(1))
        {
            let now = Instant::now();
            let raw = device.read_adc(9).await.context(
                "device stopped responding; unplug/replug the screen-side USB to recover",
            )?;
            let sample_cost = now.elapsed();
            stats.samples += 1;
            stats.sample_total += sample_cost;
            stats.sample_max = stats.sample_max.max(sample_cost);

            let gesture = button.sample(raw, now);

            if cli.trace {
                println!(
                    "raw={raw:>5} filtered={:>5} baseline={:>5} pressed={:<5} progress={:.2}",
                    button.last_filtered(),
                    button.baseline(),
                    button.is_pressed(),
                    button.hold_progress(now)
                );
            }

            if let Some(gesture) = gesture {
                println!(
                    "gesture={gesture:?} raw={raw} baseline={} pressed={}",
                    button.baseline(),
                    button.is_pressed()
                );
                state.apply(gesture);
            }
            if !calibration_announced && button.is_calibrated() {
                println!(
                    "touch pad calibrated: baseline={} spread={}",
                    button.baseline(),
                    button.calibration_spread()
                );
                if button.calibration_spread() > 60 {
                    println!(
                        "warning: samples were noisy during calibration; restart and keep hands off the back"
                    );
                }
                calibration_announced = true;
            }
        }

        refresh_ui(&ui, &state, &button, Instant::now());

        let mut rendered = false;
        window.draw_if_needed(|renderer| {
            renderer.render(&mut pixels, WIDTH);
            rendered = true;
        });

        if rendered {
            copy_pixels(&pixels, &mut current);
            let dirty = grid.diff(&previous, &current).context("tile diff failed")?;

            if !dirty.is_empty() {
                let rects = grid.merge_tiles(&dirty);
                stats.dirty_tiles += dirty.len() as u64;
                stats.rects += rects.len() as u64;

                if let Some(device) = device.as_mut() {
                    let bytes = send_tiles(device, &current, &rects).await?;
                    stats.bytes += bytes as u64;
                    stats.sent += 1;

                    if cli.verbose {
                        println!(
                            "frame={frame_index:>5} dirty_tiles={} rects={} bytes={bytes}",
                            dirty.len(),
                            rects.len()
                        );
                    }
                } else if cli.verbose {
                    println!(
                        "frame={frame_index:>5} dirty_tiles={} rects={}",
                        dirty.len(),
                        rects.len()
                    );
                }
            }

            previous.copy_from_slice(&current);
            stats.rendered += 1;
        }

        frame_index += 1;
        next_frame += interval;
        let now = Instant::now();
        if next_frame > now {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {
                    println!("\ninterrupted");
                    break;
                }
                _ = tokio::time::sleep(next_frame - now) => {}
            }
        } else if next_frame + interval < now {
            // 落后超过一帧就重新对齐，避免补偿性突发。
            next_frame = now;
        }
    }

    let elapsed = started.elapsed().as_secs_f64();
    println!(
        "rendered {} frames in {elapsed:.2}s ({:.1} fps), sent {} updates, {} dirty tiles in {} rects, {} bytes",
        stats.rendered,
        stats.rendered as f64 / elapsed.max(f64::EPSILON),
        stats.sent,
        stats.dirty_tiles,
        stats.rects,
        stats.bytes
    );

    if stats.samples > 0 {
        println!(
            "sampled {} times, avg {:.1} ms, max {:.1} ms",
            stats.samples,
            stats.sample_total.as_secs_f64() * 1000.0 / stats.samples as f64,
            stats.sample_max.as_secs_f64() * 1000.0
        );
    }

    if let Some(path) = &cli.dump_bmp {
        write_bmp(path, &pixels).context("failed to write BMP")?;
        println!("frame written to {}", path.display());
    }

    Ok(())
}

async fn connect(cli: &Cli) -> anyhow::Result<Msu2Device> {
    let timeout = Duration::from_millis(cli.timeout_ms);
    let mut device = match &cli.port {
        Some(port) => Msu2Device::connect(port.clone(), cli.baud, timeout)
            .await
            .with_context(|| format!("failed to connect to MSU2 on {port}"))?,
        None => Msu2Device::auto_connect(cli.baud, timeout)
            .await
            .context("no MSU2 device found; pass --port")?,
    };
    device
        .initialize()
        .await
        .context("failed to initialize MSU2")?;
    println!("connected to {}", device.path());
    Ok(device)
}

fn copy_pixels(pixels: &[Rgb565Pixel], out: &mut [u16]) {
    for (slot, pixel) in out.iter_mut().zip(pixels) {
        *slot = pixel.0;
    }
}

async fn send_full_frame(device: &mut Msu2Device, frame: &[u16]) -> anyhow::Result<usize> {
    let bytes = rgb565_bytes(frame);
    let len = bytes.len();
    tokio::time::timeout(
        STREAM_TIMEOUT,
        device.display_frame(0, 0, WIDTH as u16, HEIGHT as u16, &bytes),
    )
    .await
    .map_err(|_| anyhow!("initial frame write timed out"))?
    .context("failed to stream the initial frame")?;
    Ok(len)
}

async fn send_tiles(
    device: &mut Msu2Device,
    frame: &[u16],
    rects: &[Rect],
) -> anyhow::Result<usize> {
    let bytes = tokio::time::timeout(
        STREAM_TIMEOUT,
        device.stream_tiles(frame, WIDTH as u16, rects),
    )
    .await
    .map_err(|_| {
        anyhow!(
            "tile write timed out after {STREAM_TIMEOUT:?}; firmware likely hung, \
             unplug/replug the screen-side USB"
        )
    })?
    .context("failed to stream tiles")?;
    Ok(bytes)
}

fn write_bmp(path: &std::path::Path, frame: &[Rgb565Pixel]) -> std::io::Result<()> {
    let row_stride = (WIDTH * 3).div_ceil(4) * 4;
    let mut bitmap = vec![0u8; row_stride * HEIGHT];
    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            let value = frame[y * WIDTH + x].0;
            let r = (((value >> 11) & 0x1f) as u32 * 255 / 31) as u8;
            let g = (((value >> 5) & 0x3f) as u32 * 255 / 63) as u8;
            let b = ((value & 0x1f) as u32 * 255 / 31) as u8;
            let offset = (HEIGHT - 1 - y) * row_stride + x * 3;
            bitmap[offset] = b;
            bitmap[offset + 1] = g;
            bitmap[offset + 2] = r;
        }
    }

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let mut file = std::fs::File::create(path)?;
    use std::io::Write;
    file.write_all(b"BM")?;
    file.write_all(&(54 + bitmap.len() as u32).to_le_bytes())?;
    file.write_all(&0u32.to_le_bytes())?;
    file.write_all(&54u32.to_le_bytes())?;
    file.write_all(&40u32.to_le_bytes())?;
    file.write_all(&(WIDTH as i32).to_le_bytes())?;
    file.write_all(&(HEIGHT as i32).to_le_bytes())?;
    file.write_all(&1u16.to_le_bytes())?;
    file.write_all(&24u16.to_le_bytes())?;
    file.write_all(&0u32.to_le_bytes())?;
    file.write_all(&(bitmap.len() as u32).to_le_bytes())?;
    file.write_all(&2835i32.to_le_bytes())?;
    file.write_all(&2835i32.to_le_bytes())?;
    file.write_all(&0u32.to_le_bytes())?;
    file.write_all(&0u32.to_le_bytes())?;
    file.write_all(&bitmap)?;
    Ok(())
}
