//! USB 连接、重连与帧发送。

use std::time::Duration;

use anyhow::{Context, anyhow};
use msu2_core::display::{Rect, rgb565_bytes};
use msu2_core::{Msu2Device, STREAM_TIMEOUT};
use slint::platform::software_renderer::Rgb565Pixel;

use crate::Cli;
use crate::{BAUDRATE, CONNECT_TIMEOUT, HEIGHT, WIDTH};

/// 建立一次连接；失败时直接返回错误，由调用方决定是否重试。
async fn connect_once(cli: &Cli) -> anyhow::Result<Msu2Device> {
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
    Ok(device)
}

/// 带退避的连接循环，直到成功或用户按 Ctrl-C。
pub(crate) async fn connect(cli: &Cli) -> anyhow::Result<Msu2Device> {
    let mut delay = Duration::from_millis(500);

    loop {
        match connect_once(cli).await {
            Ok(device) => {
                println!("connected to {}", device.path());
                return Ok(device);
            }
            Err(error) => {
                eprintln!("connect failed: {error:#}; retrying in {delay:?}");
            }
        }

        tokio::select! {
            result = tokio::signal::ctrl_c() => {
                result.ok();
                anyhow::bail!("interrupted while reconnecting");
            }
            _ = tokio::time::sleep(delay) => {}
        }

        delay = (delay * 2).min(Duration::from_secs(5));
    }
}

pub(crate) fn copy_pixels(pixels: &[Rgb565Pixel], out: &mut [u16]) {
    for (slot, pixel) in out.iter_mut().zip(pixels) {
        *slot = pixel.0;
    }
}

pub(crate) async fn send_full_frame(
    device: &mut Msu2Device,
    frame: &[u16],
) -> anyhow::Result<usize> {
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

pub(crate) async fn send_tiles(
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
