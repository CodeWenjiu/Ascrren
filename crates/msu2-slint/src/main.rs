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

use std::time::{Duration, Instant};

use anyhow::Context;
use clap::Parser;
use msu2_core::display::{DISPLAY_HEIGHT, DISPLAY_WIDTH, TileGrid};
use msu2_core::{ButtonConfig, ButtonState};
use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType, Rgb565Pixel};

mod app;
mod metrics;
mod signin;
mod usb;

use app::{AppState, Stats, refresh_metrics, refresh_page, refresh_text};
use metrics::Metrics;
use usb::{connect, copy_pixels, send_full_frame, send_tiles};

slint::include_modules!();

pub(crate) const WIDTH: usize = DISPLAY_WIDTH as usize;
pub(crate) const HEIGHT: usize = DISPLAY_HEIGHT as usize;
pub(crate) const TILE_SIZE: u16 = 16;

/// USB CDC 下波特率只是名义值。
pub(crate) const BAUDRATE: u32 = 19200;
/// 握手和命令的默认超时。
pub(crate) const CONNECT_TIMEOUT: Duration = Duration::from_secs(8);
/// 空闲时的采样频率，单位 Hz。
const IDLE_SAMPLE_HZ: u64 = 30;
/// 按住或等待恢复时的采样频率，单位 Hz。
const ACTIVE_SAMPLE_HZ: u64 = 60;
/// 系统指标的刷新间隔。
const METRICS_INTERVAL: Duration = Duration::from_secs(1);

#[derive(Debug, Parser)]
#[command(name = "msu2-slint", about = "把 Slint 界面实时镜像到 MSU2 副屏")]
pub(crate) struct Cli {
    /// 串口路径；省略时自动查找 VID:PID 1a86:fe0c
    #[arg(long)]
    pub(crate) port: Option<String>,

    /// 目标帧率
    #[arg(long, default_value_t = 60)]
    pub(crate) fps: u64,

    /// 运行时长（毫秒）；0 表示一直运行到 Ctrl-C
    #[arg(long, default_value_t = 0)]
    pub(crate) duration_ms: u64,

    /// 判定按下的下降幅度（相对基线）；灵敏度不够时调小
    #[arg(long, default_value_t = 150)]
    pub(crate) press_delta: u16,

    /// UCAS 学号；提供后启用签到页
    #[arg(long)]
    pub(crate) ucas_user: Option<String>,

    /// UCAS 密码；省略时使用默认初始密码
    #[arg(long)]
    pub(crate) ucas_password: Option<String>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    let window = MinimalSoftwareWindow::new(RepaintBufferType::ReusedBuffer);
    app::install_platform(window.clone());

    let ui = AppWindow::new().context("failed to build the Slint UI")?;
    window.set_size(slint::PhysicalSize::new(WIDTH as u32, HEIGHT as u32));
    ui.show().context("failed to show the Slint window")?;

    let mut device = connect(&cli).await?;

    let grid = TileGrid::new(WIDTH as u16, HEIGHT as u16, TILE_SIZE)
        .context("failed to build the tile grid")?;
    let mut pixels = vec![Rgb565Pixel(0); WIDTH * HEIGHT];
    let mut current = vec![0u16; WIDTH * HEIGHT];
    let mut previous = vec![0u16; WIDTH * HEIGHT];

    // 只给学号时使用上游默认初始密码（未改过密码的账号通用）。
    let mut signin_rx = match &cli.ucas_user {
        Some(user) => {
            let password = cli
                .ucas_password
                .clone()
                .unwrap_or_else(|| course_signin::config::DEFAULT_PASSWORD.to_owned());
            let (tx, rx) = tokio::sync::mpsc::channel(4);
            signin::spawn(user.clone(), password, tx);
            println!("sign-in enabled for {user}");
            Some(rx)
        }
        None => {
            if cli.ucas_password.is_some() {
                anyhow::bail!("--ucas-password 需要配合 --ucas-user 使用");
            }
            None
        }
    };
    let mut signin_snapshot: Option<signin::Snapshot> = None;

    let mut state = AppState::new();
    state.rebuild(signin_rx.as_ref().map(|_| 0));
    let button_config = ButtonConfig {
        press_delta: cli.press_delta,
        release_delta: (u32::from(cli.press_delta) * 2 / 3) as u16,
        ..ButtonConfig::default()
    };
    let mut button = ButtonState::new(button_config);
    let mut metrics = Metrics::new();

    slint::platform::update_timers_and_animations();
    refresh_page(&ui, &state, &button);
    refresh_metrics(&ui, &metrics);
    refresh_text(&ui, &state, signin_snapshot.as_ref());
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
    let mut next_metrics = Instant::now() + METRICS_INTERVAL;
    let mut frame_index = 1u64;
    let mut calibration_announced = false;

    println!("calibrating the touch pad; keep hands off the back of the screen");

    loop {
        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            break;
        }

        slint::platform::update_timers_and_animations();

        // 每秒刷新一次时间/负载/速率和文本页。
        if Instant::now() >= next_metrics {
            metrics.refresh();
            refresh_metrics(&ui, &metrics);
            refresh_text(&ui, &state, signin_snapshot.as_ref());
            next_metrics += METRICS_INTERVAL;
        }

        // 接收后台签到任务的快照；页数随课程数变化。
        if let Some(rx) = signin_rx.as_mut() {
            while let Ok(snapshot) = rx.try_recv() {
                println!(
                    "signin: {} courses{}",
                    snapshot.courses.len(),
                    snapshot
                        .error
                        .as_deref()
                        .map(|error| format!(", error: {error}"))
                        .unwrap_or_default()
                );
                let count = snapshot.courses.len();
                signin_snapshot = Some(snapshot);
                state.rebuild(Some(count));
                refresh_page(&ui, &state, &button);
                refresh_text(&ui, &state, signin_snapshot.as_ref());
            }
        }

        // 按键采样就是保活：每次 read_adc 都是一次完整的 USB 往返。
        // 空闲时降低频率，按住或等待电平恢复时恢复高频。
        let sample_divisor = if button.needs_fast_sampling() {
            active_divisor
        } else {
            idle_divisor
        };
        if frame_index.is_multiple_of(sample_divisor) {
            let now = Instant::now();
            match device.read_adc(9).await {
                Ok(raw) => {
                    if let Some(press) = button.sample(raw, now) {
                        state.advance();
                        refresh_page(&ui, &state, &button);
                        refresh_text(&ui, &state, signin_snapshot.as_ref());
                        println!(
                            "press page={} trigger={} baseline={}",
                            state.label(),
                            press.trigger_raw,
                            button.baseline()
                        );
                    }
                    if !calibration_announced && button.is_calibrated() {
                        println!("touch pad calibrated: baseline={}", button.baseline());
                        calibration_announced = true;
                    }
                }
                Err(error) => {
                    eprintln!("touch sample failed: {error}; reconnecting");
                    device = connect(&cli).await?;
                    stats.reconnects += 1;
                    button = ButtonState::new(button_config);
                    calibration_announced = false;
                    send_full_frame(&mut device, &current).await?;
                    println!("calibrating the touch pad; keep hands off the back of the screen");
                }
            }
        }

        refresh_page(&ui, &state, &button);

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

                match send_tiles(&mut device, &current, &rects).await {
                    Ok(bytes) => {
                        stats.bytes += bytes as u64;
                        stats.sent += 1;
                    }
                    Err(error) => {
                        eprintln!("frame write failed: {error:#}; reconnecting");
                        device = connect(&cli).await?;
                        stats.reconnects += 1;
                        button = ButtonState::new(button_config);
                        calibration_announced = false;
                        previous.copy_from_slice(&current);
                        send_full_frame(&mut device, &current).await?;
                        println!(
                            "calibrating the touch pad; keep hands off the back of the screen"
                        );
                    }
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
        "rendered {} frames in {elapsed:.2}s ({:.1} fps), sent {} updates, {} dirty tiles in {} rects, {} bytes, {} reconnects",
        stats.rendered,
        stats.rendered as f64 / elapsed.max(f64::EPSILON),
        stats.sent,
        stats.dirty_tiles,
        stats.rects,
        stats.bytes,
        stats.reconnects
    );

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use course_signin::model::Course;
    use signin::Snapshot;

    fn sample_course(name: &str, begin: &str, end: &str, status: &str) -> Course {
        Course {
            id: name.to_owned(),
            uuid: name.to_owned(),
            course_name: name.to_owned(),
            teacher_name: String::new(),
            week_day: "1".to_owned(),
            class_begin_time: format!("2099-01-01 {begin}:00"),
            class_end_time: format!("2099-01-01 {end}:00"),
            sign_status: status.to_owned(),
        }
    }

    #[test]
    fn renders_all_pages() {
        let window = MinimalSoftwareWindow::new(RepaintBufferType::ReusedBuffer);
        app::install_platform(window.clone());

        let ui = AppWindow::new().expect("ui");
        window.set_size(slint::PhysicalSize::new(WIDTH as u32, HEIGHT as u32));
        ui.show().expect("show");

        let mut pixels = vec![Rgb565Pixel(0); WIDTH * HEIGHT];
        let button = ButtonState::new(ButtonConfig::default());
        let mut metrics = Metrics::new();
        metrics.time = "14:32:07".to_owned();
        metrics.date = "2026-11-26".to_owned();
        metrics.cpu_percent = 23;
        metrics.mem_percent = 61;
        metrics.net_down = "1.2 MB/s".to_owned();
        metrics.net_up = "340 KB/s".to_owned();

        let snapshot = Snapshot {
            courses: vec![
                sample_course("高等数学", "10:25", "12:00", "0"),
                sample_course("大学物理", "14:00", "15:40", "0"),
                sample_course("英语", "16:30", "18:00", "1"),
                sample_course("体育", "18:30", "20:00", "0"),
            ],
            error: None,
            updated: Some("09:01".to_owned()),
        };

        let mut state = AppState::new();
        state.rebuild(Some(snapshot.courses.len()));

        std::fs::create_dir_all("target/msu2_dump").expect("dump dir");

        // 概览 + 签到 + ceil(4/3) = 2 个课程页，共 4 页。
        for page in 0..4 {
            refresh_page(&ui, &state, &button);
            refresh_metrics(&ui, &metrics);
            refresh_text(&ui, &state, Some(&snapshot));
            slint::platform::update_timers_and_animations();
            window.draw_if_needed(|renderer| {
                renderer.render(&mut pixels, WIDTH);
            });
            write_bmp(&format!("target/msu2_dump/page{page}.bmp"), &pixels).expect("bmp");
            state.advance();
        }
    }

    fn write_bmp(path: &str, frame: &[Rgb565Pixel]) -> std::io::Result<()> {
        use std::io::Write;

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

        let mut file = std::fs::File::create(path)?;
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
}
