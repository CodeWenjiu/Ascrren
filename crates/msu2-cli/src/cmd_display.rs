//! 屏幕内容类命令：填充、整帧、图片、字符和清屏。

use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{Context, bail};
use msu2_core::Msu2Device;

pub(crate) struct FillSpec {
    pub(crate) x: u16,
    pub(crate) y: u16,
    pub(crate) width: u16,
    pub(crate) height: u16,
    pub(crate) color: String,
}

pub(crate) async fn cmd_fill(
    device: &mut Msu2Device,
    spec: &FillSpec,
    force: bool,
    hold_ms: u64,
) -> anyhow::Result<()> {
    if !force {
        bail!("fill uses the experimental streaming path; pass --force to try it");
    }
    if spec.width == 0 || spec.height == 0 {
        bail!("width and height must be greater than zero");
    }
    if spec.width < 16 || spec.height < 16 {
        bail!(
            "width and height must be at least 16; tiny windows hit a firmware LCD/DMA edge case"
        );
    }
    if spec.x.saturating_add(spec.width) > 160 || spec.y.saturating_add(spec.height) > 80 {
        bail!("rectangle must fit inside the 160x80 screen");
    }

    let rgb565 = parse_rgb565(&spec.color)?;
    device
        .initialize()
        .await
        .context("failed to initialize MSU2")?;
    device
        .fill_rect_words(spec.x, spec.y, spec.width, spec.height, rgb565)
        .await
        .context("failed to stream fill-word data")?;
    tokio::time::sleep(Duration::from_millis(50)).await;

    println!(
        "filled {}x{} at ({}, {}) with #{} (RGB565 0x{rgb565:04X})",
        spec.width,
        spec.height,
        spec.x,
        spec.y,
        spec.color.trim_start_matches('#').to_uppercase()
    );

    hold_session(device, hold_ms).await?;
    Ok(())
}

pub(crate) struct FrameSpec {
    pub(crate) x: u16,
    pub(crate) y: u16,
    pub(crate) width: u16,
    pub(crate) height: u16,
}

pub(crate) async fn cmd_frame(
    device: &mut Msu2Device,
    file: &PathBuf,
    spec: FrameSpec,
    force: bool,
    hold_ms: u64,
) -> anyhow::Result<()> {
    if !force {
        bail!("raw framebuffer streaming is experimental; pass --force to try it");
    }
    if spec.x.saturating_add(spec.width) > 160 || spec.y.saturating_add(spec.height) > 80 {
        bail!("frame must fit inside the 160x80 screen");
    }

    let data = std::fs::read(file)
        .with_context(|| format!("failed to read frame file {}", file.display()))?;
    let expected = usize::from(spec.width) * usize::from(spec.height) * 2;
    if data.len() != expected {
        bail!(
            "frame file has {} bytes, expected {expected} for {}x{} RGB565",
            data.len(),
            spec.width,
            spec.height
        );
    }

    device
        .initialize()
        .await
        .context("failed to initialize MSU2")?;
    device
        .display_frame(spec.x, spec.y, spec.width, spec.height, &data)
        .await
        .context("failed to stream frame")?;
    tokio::time::sleep(Duration::from_millis(50)).await;

    println!(
        "sent {}x{} frame from {}",
        spec.width,
        spec.height,
        file.display()
    );
    hold_session(device, hold_ms).await?;
    Ok(())
}

/// 保持连接一段时间，期间只读 ADC 保活。
async fn hold_session(device: &mut Msu2Device, hold_ms: u64) -> anyhow::Result<()> {
    if hold_ms == 0 {
        return Ok(());
    }

    let deadline = Instant::now() + Duration::from_millis(hold_ms);
    let mut failures = 0u32;
    while Instant::now() < deadline {
        // 只保活，不重复发送流式数据，避免放大固件风险。
        match device.keepalive().await {
            Ok(()) => failures = 0,
            Err(error) => {
                failures += 1;
                if failures >= 2 {
                    bail!(
                        "device stopped responding after streaming ({error}); \
                         firmware likely hung, unplug/replug USB to recover"
                    );
                }
            }
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    println!("held session for {hold_ms} ms");
    Ok(())
}

pub(crate) async fn cmd_photo(
    device: &mut Msu2Device,
    page: u16,
    x: u16,
    y: u16,
    width: u16,
    height: u16,
) -> anyhow::Result<()> {
    if width == 0 || height == 0 {
        bail!("width and height must be greater than zero");
    }
    if x.saturating_add(width) > 160 || y.saturating_add(height) > 80 {
        bail!("image must fit inside the 160x80 screen");
    }

    device
        .initialize()
        .await
        .context("failed to initialize MSU2")?;
    device
        .display_flash_image(x, y, width, height, page)
        .await
        .context("failed to display flash image")?;
    tokio::time::sleep(Duration::from_millis(500)).await;

    println!("displayed flash page {page} at ({x}, {y}) {width}x{height}");
    Ok(())
}

pub(crate) async fn cmd_ascii(
    device: &mut Msu2Device,
    text: &str,
    x: u16,
    y: u16,
    fg: &str,
    bg: &str,
    font_page: u16,
) -> anyhow::Result<()> {
    let bytes = text.as_bytes();
    if bytes.len() != 1 || !bytes[0].is_ascii() {
        bail!("--text must be exactly one ASCII character");
    }
    let fg = parse_rgb565(fg)?;
    let bg = parse_rgb565(bg)?;

    device
        .initialize()
        .await
        .context("failed to initialize MSU2")?;
    device
        .display_ascii(x, y, bytes[0], fg, bg, font_page)
        .await
        .context("failed to display ASCII glyph")?;
    tokio::time::sleep(Duration::from_millis(300)).await;

    println!("displayed ASCII {text:?} at ({x}, {y}) font_page={font_page}");
    Ok(())
}

pub(crate) async fn cmd_clear(
    device: &mut Msu2Device,
    bg: &str,
    font_page: u16,
) -> anyhow::Result<()> {
    let color = parse_rgb565(bg)?;

    device
        .initialize()
        .await
        .context("failed to initialize MSU2")?;

    for y in (0..80).step_by(64) {
        for x in (0..160).step_by(32) {
            device
                .display_ascii(x, y, b' ', 0, color, font_page)
                .await
                .context("failed to clear cell")?;
        }
    }

    tokio::time::sleep(Duration::from_millis(300)).await;
    println!("cleared screen with background #{bg}");
    Ok(())
}

#[derive(Clone, Copy)]
pub(crate) struct TextStyle {
    pub(crate) x: u16,
    pub(crate) y: u16,
    pub(crate) fg: u16,
    pub(crate) bg: u16,
    pub(crate) font_page: u16,
    pub(crate) spacing: u16,
    pub(crate) line_height: u16,
}

pub(crate) async fn cmd_text(
    device: &mut Msu2Device,
    text: &str,
    style: TextStyle,
    hold_ms: u64,
    refresh_ms: u64,
) -> anyhow::Result<()> {
    device
        .initialize()
        .await
        .context("failed to initialize MSU2")?;

    draw_text(device, text, style).await?;

    if hold_ms > 0 {
        let deadline = Instant::now() + Duration::from_millis(hold_ms);
        let mut next_refresh = Instant::now() + Duration::from_millis(refresh_ms.max(1));

        while Instant::now() < deadline {
            // 与厂商 Demo 类似：周期读 ADC，保持主机会话。
            let _ = device.keepalive().await;

            if refresh_ms > 0 && Instant::now() >= next_refresh {
                draw_text(device, text, style).await?;
                next_refresh = Instant::now() + Duration::from_millis(refresh_ms);
            }

            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }

    tokio::time::sleep(Duration::from_millis(300)).await;
    println!("displayed text {text:?} at ({}, {})", style.x, style.y);
    Ok(())
}

async fn draw_text(device: &mut Msu2Device, text: &str, style: TextStyle) -> anyhow::Result<()> {
    let glyph_width = 32u16;
    let mut cursor_x = style.x;
    let mut cursor_y = style.y;

    for byte in text.bytes() {
        if byte == b'\n' {
            cursor_x = style.x;
            cursor_y = cursor_y.saturating_add(style.line_height);
            continue;
        }
        if byte == b' ' {
            cursor_x = cursor_x.saturating_add(glyph_width + style.spacing);
            continue;
        }
        if !byte.is_ascii() {
            bail!("only ASCII text is supported by the built-in 32x64 font");
        }

        if cursor_x.saturating_add(glyph_width) > 160 {
            cursor_x = style.x;
            cursor_y = cursor_y.saturating_add(style.line_height);
        }

        device
            .display_ascii(
                cursor_x,
                cursor_y,
                byte,
                style.fg,
                style.bg,
                style.font_page,
            )
            .await
            .context("failed to display ASCII glyph")?;
        cursor_x = cursor_x.saturating_add(glyph_width + style.spacing);
    }

    Ok(())
}
pub(crate) fn parse_rgb565(input: &str) -> anyhow::Result<u16> {
    let input = input.trim().trim_start_matches('#');
    let input = input
        .strip_prefix("0x")
        .or_else(|| input.strip_prefix("0X"))
        .unwrap_or(input);

    if input.len() != 6 || !input.chars().all(|ch| ch.is_ascii_hexdigit()) {
        bail!("color must be RRGGBB, e.g. FF0000");
    }

    let rgb = u32::from_str_radix(input, 16).context("invalid color")?;
    let red = ((rgb >> 16) & 0xFF) as u16;
    let green = ((rgb >> 8) & 0xFF) as u16;
    let blue = (rgb & 0xFF) as u16;

    Ok(((red >> 3) << 11) | ((green >> 2) << 5) | (blue >> 3))
}
