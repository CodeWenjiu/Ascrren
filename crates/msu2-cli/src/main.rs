use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{Context, bail};
use clap::{Parser, Subcommand};
use msu2_core::{Msu2Device, SfrValue};

#[derive(Debug, Parser)]
#[command(name = "msu2", version, about = "MSU2 USB 副屏跨平台命令行工具")]
struct Cli {
    /// 串口路径；省略时自动查找 VID:PID 1a86:fe0c
    #[arg(long, global = true)]
    port: Option<String>,

    /// 串口波特率（USB CDC 下通常只是名义值）
    #[arg(long, global = true, default_value_t = 19200)]
    baud: u32,

    /// 命令/握手超时时间（毫秒）；固件在主机断开后需要几秒恢复心跳
    #[arg(long, global = true, default_value_t = 8000)]
    timeout_ms: u64,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// 列出串口并标记 MSU2 设备
    Ports,

    /// 握手并打印设备信息、SFR 表
    Info,

    /// 读取背面触摸键（ADC 通道 9）
    Key {
        /// 采样次数
        #[arg(long, default_value_t = 10)]
        count: u32,

        /// 采样间隔（毫秒）
        #[arg(long, default_value_t = 200)]
        interval_ms: u64,
    },

    /// 用纯色填充屏幕区域（使用 02 04 填充字路径）
    Fill {
        #[arg(long, default_value_t = 0)]
        x: u16,

        #[arg(long, default_value_t = 0)]
        y: u16,

        #[arg(long, default_value_t = 160)]
        width: u16,

        #[arg(long, default_value_t = 80)]
        height: u16,

        /// RRGGBB，例如 FF0000
        #[arg(long, default_value = "000000")]
        color: String,

        /// 明知会卡死固件仍要尝试（仅用于调试）
        #[arg(long)]
        force: bool,

        /// 发送后保持连接的时长（毫秒）；用于观察画面是否稳定
        #[arg(long, default_value_t = 0)]
        hold_ms: u64,
    },

    /// 发送原始 RGB565 帧（160×80，小端文件字节序按 RGB565 大端写入）
    Frame {
        /// 原始 RGB565 文件，长度必须为 160×80×2 字节
        #[arg(long, value_name = "FILE")]
        file: PathBuf,

        #[arg(long, default_value_t = 0)]
        x: u16,

        #[arg(long, default_value_t = 0)]
        y: u16,

        #[arg(long, default_value_t = 160)]
        width: u16,

        #[arg(long, default_value_t = 80)]
        height: u16,

        /// 明知流式路径有风险仍要尝试
        #[arg(long)]
        force: bool,

        /// 发送后保持连接的时长（毫秒）
        #[arg(long, default_value_t = 0)]
        hold_ms: u64,
    },

    /// 显示外部 Flash 中预存的彩色图片
    Photo {
        /// Flash 页号；0 对应出厂图片 1
        #[arg(long, default_value_t = 0)]
        page: u16,

        #[arg(long, default_value_t = 0)]
        x: u16,

        #[arg(long, default_value_t = 0)]
        y: u16,

        #[arg(long, default_value_t = 160)]
        width: u16,

        #[arg(long, default_value_t = 80)]
        height: u16,
    },

    /// 用内置 32×64 字库显示一个 ASCII 字符
    Ascii {
        /// 单个 ASCII 字符
        #[arg(long, default_value = "A")]
        text: String,

        #[arg(long, default_value_t = 10)]
        x: u16,

        #[arg(long, default_value_t = 10)]
        y: u16,

        /// 前景色 RRGGBB
        #[arg(long, default_value = "FFFF00")]
        fg: String,

        /// 背景色 RRGGBB
        #[arg(long, default_value = "000000")]
        bg: String,

        /// ASCII 字库在 Flash 中的起始页
        #[arg(long, default_value_t = 3651)]
        font_page: u16,
    },

    /// 用内置 32×64 ASCII 字库显示多字符文本
    Text {
        /// 文本；支持空格和 \n 换行
        #[arg(long)]
        text: String,

        #[arg(long, default_value_t = 0)]
        x: u16,

        #[arg(long, default_value_t = 0)]
        y: u16,

        /// 前景色 RRGGBB
        #[arg(long, default_value = "FFFF00")]
        fg: String,

        /// 背景色 RRGGBB
        #[arg(long, default_value = "000000")]
        bg: String,

        /// ASCII 字库在 Flash 中的起始页
        #[arg(long, default_value_t = 3651)]
        font_page: u16,

        /// 字符间距
        #[arg(long, default_value_t = 0)]
        spacing: u16,

        /// 行高
        #[arg(long, default_value_t = 72)]
        line_height: u16,

        /// 发送后保持连接的时长（毫秒）；0 表示立即退出
        #[arg(long, default_value_t = 0)]
        hold_ms: u64,

        /// 保持期间的重绘间隔（毫秒）；0 表示不重绘
        #[arg(long, default_value_t = 1000)]
        refresh_ms: u64,
    },

    /// 用 ASCII 空格字形清屏（安全，不使用流式 DMA）
    Clear {
        /// 背景色 RRGGBB
        #[arg(long, default_value = "000000")]
        bg: String,

        /// ASCII 字库在 Flash 中的起始页
        #[arg(long, default_value_t = 3651)]
        font_page: u16,
    },
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("error: {error:#}");
        std::process::exit(1);
    }
}

async fn run() -> anyhow::Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Command::Ports => cmd_ports(),
        Command::Info => {
            let mut device = connect(&cli).await?;
            cmd_info(&mut device).await
        }
        Command::Key { count, interval_ms } => {
            let mut device = connect(&cli).await?;
            cmd_key(&mut device, count, interval_ms).await
        }
        Command::Fill {
            x,
            y,
            width,
            height,
            ref color,
            force,
            hold_ms,
        } => {
            let mut device = connect(&cli).await?;
            let spec = FillSpec {
                x,
                y,
                width,
                height,
                color: color.clone(),
            };
            cmd_fill(&mut device, &spec, force, hold_ms).await
        }
        Command::Frame {
            ref file,
            x,
            y,
            width,
            height,
            force,
            hold_ms,
        } => {
            let mut device = connect(&cli).await?;
            cmd_frame(
                &mut device,
                file,
                FrameSpec {
                    x,
                    y,
                    width,
                    height,
                },
                force,
                hold_ms,
            )
            .await
        }
        Command::Photo {
            page,
            x,
            y,
            width,
            height,
        } => {
            let mut device = connect(&cli).await?;
            cmd_photo(&mut device, page, x, y, width, height).await
        }
        Command::Ascii {
            ref text,
            x,
            y,
            ref fg,
            ref bg,
            font_page,
        } => {
            let mut device = connect(&cli).await?;
            cmd_ascii(&mut device, text, x, y, fg, bg, font_page).await
        }
        Command::Text {
            ref text,
            x,
            y,
            ref fg,
            ref bg,
            font_page,
            spacing,
            line_height,
            hold_ms,
            refresh_ms,
        } => {
            let mut device = connect(&cli).await?;
            let style = TextStyle {
                x,
                y,
                fg: parse_rgb565(fg)?,
                bg: parse_rgb565(bg)?,
                font_page,
                spacing,
                line_height,
            };
            cmd_text(&mut device, text, style, hold_ms, refresh_ms).await
        }
        Command::Clear { ref bg, font_page } => {
            let mut device = connect(&cli).await?;
            cmd_clear(&mut device, bg, font_page).await
        }
    }
}

async fn connect(cli: &Cli) -> anyhow::Result<Msu2Device> {
    let path = resolve_port(cli)?;
    let timeout = Duration::from_millis(cli.timeout_ms);

    Msu2Device::connect(path.clone(), cli.baud, timeout)
        .await
        .with_context(|| {
            format!(
                "failed to connect to MSU2 on {path}; check the USB cable, permissions \
                 (dialout/uucp group or udev rule), and whether another program owns the port"
            )
        })
}

fn resolve_port(cli: &Cli) -> anyhow::Result<String> {
    if let Some(port) = &cli.port {
        return Ok(port.clone());
    }

    let info = msu2_core::find_msu2_port()
        .context("no MSU2 device found; run `msu2 ports` or pass --port")?;
    Ok(info.path)
}

fn cmd_ports() -> anyhow::Result<()> {
    let ports = msu2_core::list_ports().context("failed to enumerate serial ports")?;

    if ports.is_empty() {
        println!("No serial ports found.");
        return Ok(());
    }

    println!(
        "{:<24} {:<11} {:<24} {:<18} MSU2",
        "PORT", "VID:PID", "PRODUCT", "SERIAL"
    );
    for port in ports {
        println!(
            "{:<24} {:<11} {:<24} {:<18} {}",
            port.path,
            port.vid_pid(),
            port.product.as_deref().unwrap_or("-"),
            port.serial_number.as_deref().unwrap_or("-"),
            if port.is_msu2() { "yes" } else { "" }
        );
    }

    Ok(())
}

async fn cmd_info(device: &mut Msu2Device) -> anyhow::Result<()> {
    println!("Port: {}", device.path());
    println!("Firmware: {}", device.version_string());

    if let Ok(ports) = msu2_core::list_ports() {
        if let Some(port) = ports
            .iter()
            .find(|port| port.path.as_str() == device.path())
        {
            println!(
                "USB: {} {}",
                port.vid_pid(),
                port.product.as_deref().unwrap_or("")
            );
        }
    }

    let table = device
        .read_sfr_table()
        .await
        .context("failed to read SFR table")?;
    println!("SFR entries: {}", table.entries.len());

    for entry in &table.entries {
        let value = match device.read_named(&table, &entry.name).await {
            Ok(value) => format_sfr_value(&value),
            Err(error) => format!("<error: {error}>"),
        };
        println!(
            "  {:<14} {:<20} {}",
            entry.name,
            entry.kind.description(),
            value
        );
    }

    Ok(())
}

async fn cmd_key(device: &mut Msu2Device, count: u32, interval_ms: u64) -> anyhow::Result<()> {
    let first = device
        .read_adc(9)
        .await
        .context("failed to read ADC channel 9")?;
    let second = device
        .read_adc(9)
        .await
        .context("failed to read ADC channel 9")?;

    let baseline = ((u32::from(first) + u32::from(second)) / 2) as u16;
    let threshold = baseline.saturating_sub(125);
    println!("ADC9 baseline={baseline}, press threshold={threshold}");

    for index in 0..count {
        let value = device.read_adc(9).await?;
        let state = if value < threshold {
            "pressed"
        } else {
            "released"
        };
        println!("[{index:>3}] ADC9={value:>4}  {state}");

        if index + 1 < count {
            tokio::time::sleep(Duration::from_millis(interval_ms)).await;
        }
    }

    Ok(())
}

struct FillSpec {
    x: u16,
    y: u16,
    width: u16,
    height: u16,
    color: String,
}

async fn cmd_fill(
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

struct FrameSpec {
    x: u16,
    y: u16,
    width: u16,
    height: u16,
}

async fn cmd_frame(
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

async fn cmd_photo(
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

async fn cmd_ascii(
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

async fn cmd_clear(device: &mut Msu2Device, bg: &str, font_page: u16) -> anyhow::Result<()> {
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
struct TextStyle {
    x: u16,
    y: u16,
    fg: u16,
    bg: u16,
    font_page: u16,
    spacing: u16,
    line_height: u16,
}

async fn cmd_text(
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

fn format_sfr_value(value: &SfrValue) -> String {
    match value {
        SfrValue::U16(value) => format!("{value} (0x{value:04X})"),
        SfrValue::Bytes(bytes) => hex_bytes(bytes),
        SfrValue::Text(text) => format!("{text:?}"),
    }
}

fn hex_bytes(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|byte| format!("{byte:02X}"))
        .collect::<Vec<_>>()
        .join(" ")
}

fn parse_rgb565(input: &str) -> anyhow::Result<u16> {
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
