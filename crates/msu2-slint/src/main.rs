//! 长驻的 Slint → MSU2 实时显示桥。
//!
//! Slint 用 `MinimalSoftwareWindow` 软件渲染到 160×80 RGB565 framebuffer，
//! 每帧与上一帧做 tile 差分，只把变化的矩形通过 USB 发给屏幕。背面触摸
//! 焊盘用于翻页，采样顺便充当保活，避免固件回到出场动画。
//!
//! ```text
//! msu2-slint                  # 自动找设备，60 fps，直到 Ctrl-C
//! msu2-slint --fps 30         # 限制帧率
//! ```

use std::rc::Rc;
use std::time::{Duration, Instant};

use anyhow::{Context, anyhow};
use clap::Parser;
use msu2_core::display::{DISPLAY_HEIGHT, DISPLAY_WIDTH, Rect, TileGrid, rgb565_bytes};
use msu2_core::{ButtonConfig, ButtonState, Msu2Device, STREAM_TIMEOUT};
use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType, Rgb565Pixel};
use slint::platform::{Platform, WindowAdapter};

slint::include_modules!();

const WIDTH: usize = DISPLAY_WIDTH as usize;
const HEIGHT: usize = DISPLAY_HEIGHT as usize;
const TILE_SIZE: u16 = 16;

/// 循环展示的页面数。
const PAGES: usize = 4;
/// USB CDC 下波特率只是名义值。
const BAUDRATE: u32 = 19200;
/// 握手和命令的默认超时。
const CONNECT_TIMEOUT: Duration = Duration::from_secs(8);
/// 空闲时的采样频率，单位 Hz。
const IDLE_SAMPLE_HZ: u64 = 15;
/// 按住或等待恢复时的采样频率，单位 Hz。
const ACTIVE_SAMPLE_HZ: u64 = 30;

#[derive(Debug, Parser)]
#[command(name = "msu2-slint", about = "把 Slint 界面实时镜像到 MSU2 副屏")]
struct Cli {
    /// 串口路径；省略时自动查找 VID:PID 1a86:fe0c
    #[arg(long)]
    port: Option<String>,

    /// 目标帧率
    #[arg(long, default_value_t = 60)]
    fps: u64,

    /// 运行时长（毫秒）；0 表示一直运行到 Ctrl-C
    #[arg(long, default_value_t = 0)]
    duration_ms: u64,

    /// 判定按下的下降幅度（相对基线）；灵敏度不够时调小
    #[arg(long, default_value_t = 150)]
    press_delta: u16,
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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct AppState {
    page: usize,
}

impl AppState {
    fn new() -> Self {
        Self { page: 0 }
    }

    fn advance(&mut self) {
        self.page = (self.page + 1) % PAGES;
    }
}

fn refresh_ui(ui: &AppWindow, state: &AppState, button: &ButtonState) {
    ui.set_page(state.page as i32);
    ui.set_pressed(button.is_pressed());
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

    let mut device = connect(&cli).await?;

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
        ..ButtonConfig::default()
    });

    slint::platform::update_timers_and_animations();
    refresh_ui(&ui, &state, &button);
    window.draw_if_needed(|renderer| {
        renderer.render(&mut pixels, WIDTH);
    });
    copy_pixels(&pixels, &mut current);

    let mut stats = Stats::default();
    let bytes = send_full_frame(&mut device, &current).await?;
    stats.bytes += bytes as u64;
    stats.sent += 1;
    println!("initial frame sent ({bytes} B)");
    previous.copy_from_slice(&current);
    stats.rendered += 1;

    let interval = Duration::from_micros(1_000_000 / cli.fps.max(1));
    // 采样频率跟着帧率走，避免降帧后按键变得不灵敏。
    let idle_divisor = (cli.fps.max(1) / IDLE_SAMPLE_HZ).max(1);
    let active_divisor = (cli.fps.max(1) / ACTIVE_SAMPLE_HZ).max(1);
    let started = Instant::now();
    let deadline = (cli.duration_ms > 0).then(|| started + Duration::from_millis(cli.duration_ms));
    let mut next_frame = Instant::now() + interval;
    let mut frame_index = 1u64;
    let mut calibration_announced = false;

    println!("calibrating the touch pad; keep hands off the back of the screen");

    loop {
        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            break;
        }

        slint::platform::update_timers_and_animations();

        // 按键采样就是保活：每次 read_adc 都是一次完整的 USB 往返。
        // 空闲时降低频率，按住或等待电平恢复时恢复高频。
        let sample_divisor = if button.needs_fast_sampling() {
            active_divisor
        } else {
            idle_divisor
        };
        if frame_index.is_multiple_of(sample_divisor) {
            let now = Instant::now();
            let raw = device.read_adc(9).await.context(
                "device stopped responding; unplug/replug the screen-side USB to recover",
            )?;
            if let Some(press) = button.sample(raw, now) {
                state.advance();
                println!(
                    "press held={}ms page={} raw={raw} baseline={}",
                    press.held.as_millis(),
                    state.page,
                    button.baseline()
                );
            }
            if !calibration_announced && button.is_calibrated() {
                println!("touch pad calibrated: baseline={}", button.baseline());
                calibration_announced = true;
            }
        }

        refresh_ui(&ui, &state, &button);

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

                let bytes = send_tiles(&mut device, &current, &rects).await?;
                stats.bytes += bytes as u64;
                stats.sent += 1;
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

    Ok(())
}

async fn connect(cli: &Cli) -> anyhow::Result<Msu2Device> {
    let mut device = match &cli.port {
        Some(port) => Msu2Device::connect(port.clone(), BAUDRATE, CONNECT_TIMEOUT)
            .await
            .with_context(|| format!("failed to connect to MSU2 on {port}"))?,
        None => Msu2Device::auto_connect(BAUDRATE, CONNECT_TIMEOUT)
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
