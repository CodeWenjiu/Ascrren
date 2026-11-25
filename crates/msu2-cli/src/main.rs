use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::PathBuf;
use std::process::Command as ProcessCommand;
use std::time::{Duration, Instant};

use anyhow::{Context, bail};
use clap::{Parser, Subcommand};
use msu2_core::{Frame, Msu2Device, SfrValue};

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

    /// 跳过 MSNCN 握手（仅用于设备恢复/调试）
    #[arg(long, global = true)]
    no_handshake: bool,

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

    /// 用纯色填充屏幕区域（实验性：V1.0 固件的流式路径会卡死）
    Fill {
        #[arg(long, default_value_t = 0)]
        x: u16,

        #[arg(long, default_value_t = 0)]
        y: u16,

        #[arg(long, default_value_t = 240)]
        width: u16,

        #[arg(long, default_value_t = 240)]
        height: u16,

        /// RRGGBB，例如 FF0000
        #[arg(long, default_value = "000000")]
        color: String,

        /// 明知会卡死固件仍要尝试（仅用于调试）
        #[arg(long)]
        force: bool,
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

        #[arg(long, default_value_t = 240)]
        width: u16,

        #[arg(long, default_value_t = 240)]
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

    /// 用内置 16×16 字库显示一个 GB2312 字符（实验性，当前固件可能没有字库）
    Gb2312 {
        /// GB2312 双字节编码（十六进制），例如 D6D0 = 中
        #[arg(long, default_value = "D6D0")]
        code: String,

        #[arg(long, default_value_t = 10)]
        x: u16,

        #[arg(long, default_value_t = 10)]
        y: u16,

        /// 前景色 RRGGBB
        #[arg(long, default_value = "FFFFFF")]
        fg: String,

        /// 背景色 RRGGBB
        #[arg(long, default_value = "000000")]
        bg: String,
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
        #[arg(long, default_value_t = 4)]
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

    /// 读取外部 Flash 的一段字节（调试用）
    FlashRead {
        /// 起始字节地址
        #[arg(long, default_value_t = 0)]
        address: u32,

        /// 读取长度（字节）
        #[arg(long, default_value_t = 64)]
        length: u32,
    },

    /// 将外部 Flash 的一段字节原样备份到文件
    FlashDump {
        /// 起始字节地址
        #[arg(long, default_value_t = 0)]
        address: u32,

        /// 读取长度（字节）
        #[arg(long)]
        length: u32,

        /// 输出文件路径
        #[arg(long, value_name = "FILE")]
        output: PathBuf,
    },

    /// 通过 USB hub 端口断电重启 MSU2（Linux/uhubctl）
    Reset {
        /// uhubctl hub 位置，例如 3-2；省略时自动查找
        #[arg(long)]
        hub: Option<String>,

        /// hub 端口号；省略时自动查找
        #[arg(long)]
        hub_port: Option<u8>,

        /// 等待设备重新枚举的时间（毫秒）
        #[arg(long, default_value_t = 10_000)]
        wait_ms: u64,
    },

    /// 发送原始十六进制字节并打印一帧响应
    Raw {
        /// 例如 "00 30 00 01 00 00"
        #[arg(value_name = "HEX")]
        hex: String,

        /// 发送前先执行 MSNCN 握手
        #[arg(long)]
        handshake: bool,

        /// 发送命令前的等待时间（毫秒）
        #[arg(long, default_value_t = 0)]
        delay_ms: u64,
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
        } => {
            let mut device = connect(&cli).await?;
            cmd_fill(&mut device, x, y, width, height, color, force).await
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
        Command::Gb2312 {
            ref code,
            x,
            y,
            ref fg,
            ref bg,
        } => {
            let mut device = connect(&cli).await?;
            cmd_gb2312(&mut device, code, x, y, fg, bg).await
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
        Command::FlashRead { address, length } => {
            let mut device = connect(&cli).await?;
            cmd_flash_read(&mut device, address, length).await
        }
        Command::FlashDump {
            address,
            length,
            ref output,
        } => {
            let mut device = connect(&cli).await?;
            cmd_flash_dump(&mut device, address, length, output).await
        }

        Command::Raw {
            ref hex,
            handshake,
            delay_ms,
        } => cmd_raw(&cli, hex, handshake, delay_ms).await,
        Command::Reset {
            ref hub,
            hub_port,
            wait_ms,
        } => cmd_reset(&cli, hub.as_deref(), hub_port, wait_ms).await,
    }
}

async fn connect(cli: &Cli) -> anyhow::Result<Msu2Device> {
    let path = resolve_port(cli)?;
    let timeout = Duration::from_millis(cli.timeout_ms);

    if cli.no_handshake {
        Msu2Device::open(path.clone(), cli.baud, timeout).with_context(|| {
            format!(
                "failed to open MSU2 on {path}; check the USB cable, permissions \
                 (dialout/uucp group or udev rule), and whether another program owns the port"
            )
        })
    } else {
        Msu2Device::connect(path.clone(), cli.baud, timeout)
            .await
            .with_context(|| {
                format!(
                    "failed to connect to MSU2 on {path}; check the USB cable, permissions \
                     (dialout/uucp group or udev rule), and whether another program owns the port"
                )
            })
    }
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

async fn cmd_fill(
    device: &mut Msu2Device,
    x: u16,
    y: u16,
    width: u16,
    height: u16,
    color: &str,
    force: bool,
) -> anyhow::Result<()> {
    if !force {
        bail!(
            "host streaming (fill) is known to hang MSU2 firmware V1.0; \
             use `photo`, `ascii`, or `gb2312` instead, or pass --force for debugging"
        );
    }
    if width == 0 || height == 0 {
        bail!("width and height must be greater than zero");
    }
    if width < 16 || height < 16 {
        bail!(
            "width and height must be at least 16; tiny windows hit a firmware LCD/DMA edge case"
        );
    }
    if x.saturating_add(width) > 240 || y.saturating_add(height) > 240 {
        bail!("rectangle must fit inside the 240x240 screen");
    }

    let rgb565 = parse_rgb565(color)?;
    device
        .initialize()
        .await
        .context("failed to initialize MSU2")?;
    device
        .fill_rect(x, y, width, height, rgb565)
        .await
        .context("failed to stream fill data")?;
    tokio::time::sleep(Duration::from_millis(50)).await;

    println!(
        "filled {}x{} at ({}, {}) with #{} (RGB565 0x{rgb565:04X})",
        width,
        height,
        x,
        y,
        color.trim_start_matches('#').to_uppercase()
    );

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

async fn cmd_gb2312(
    device: &mut Msu2Device,
    code: &str,
    x: u16,
    y: u16,
    fg: &str,
    bg: &str,
) -> anyhow::Result<()> {
    let code = parse_hex_bytes(code)?;
    if code.len() != 2 {
        bail!("--code must be exactly two hex bytes, e.g. D6D0");
    }
    let fg = parse_rgb565(fg)?;
    let bg = parse_rgb565(bg)?;

    device
        .initialize()
        .await
        .context("failed to initialize MSU2")?;
    device
        .display_gb2312(x, y, code[0], code[1], fg, bg)
        .await
        .context("failed to display GB2312 glyph")?;
    tokio::time::sleep(Duration::from_millis(300)).await;

    println!(
        "displayed GB2312 {:02X}{:02X} at ({x}, {y})",
        code[0], code[1]
    );
    Ok(())
}

async fn cmd_clear(device: &mut Msu2Device, bg: &str, font_page: u16) -> anyhow::Result<()> {
    let color = parse_rgb565(bg)?;

    device
        .initialize()
        .await
        .context("failed to initialize MSU2")?;

    for y in (0..240).step_by(64) {
        for x in (0..240).step_by(32) {
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
            let _ = device.read_adc(9).await;

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

        if cursor_x.saturating_add(glyph_width) > 240 {
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

async fn cmd_flash_dump(
    device: &mut Msu2Device,
    address: u32,
    length: u32,
    output: &PathBuf,
) -> anyhow::Result<()> {
    if length == 0 {
        bail!("dump length must be greater than zero");
    }
    address
        .checked_add(length)
        .context("flash dump range overflows a 32-bit address")?;

    device
        .initialize()
        .await
        .context("failed to initialize MSU2")?;

    let file = File::create(output)
        .with_context(|| format!("failed to create dump file {}", output.display()))?;
    let mut writer = BufWriter::new(file);
    let mut written = 0u32;

    // 单次请求只返回 1 字节；按块写入文件，避免把整段数据堆在内存里。
    const CHUNK: u32 = 4096;
    while written < length {
        let count = CHUNK.min(length - written);
        let mut chunk = Vec::with_capacity(count as usize);
        for offset in 0..count {
            chunk.push(device.read_flash_byte(address + written + offset).await?);
        }
        writer
            .write_all(&chunk)
            .with_context(|| format!("failed to write dump file {}", output.display()))?;
        written += count;
        eprintln!("read {written}/{length} bytes");
    }
    writer.flush().context("failed to flush dump file")?;

    println!(
        "dumped {length} bytes from 0x{address:06X} to {}",
        output.display()
    );
    Ok(())
}

async fn cmd_flash_read(device: &mut Msu2Device, address: u32, length: u32) -> anyhow::Result<()> {
    device
        .initialize()
        .await
        .context("failed to initialize MSU2")?;

    let mut data = Vec::with_capacity(length as usize);
    for offset in 0..length {
        data.push(device.read_flash_byte(address + offset).await?);
    }

    for (index, chunk) in data.chunks(32).enumerate() {
        println!(
            "{:06X}: {}",
            address as usize + index * 32,
            hex_bytes(chunk)
        );
    }
    Ok(())
}

async fn cmd_reset(
    cli: &Cli,
    hub: Option<&str>,
    hub_port: Option<u8>,
    wait_ms: u64,
) -> anyhow::Result<()> {
    let wait = Duration::from_millis(wait_ms);

    // 1. 串口 BREAK：这个固件会把它当作复位信号，且不依赖 VBUS 断电。
    if let Ok(ports) = msu2_core::list_ports() {
        if let Some(port) = ports.into_iter().find(|port| port.is_msu2()) {
            println!("Sending serial BREAK to {}...", port.path);
            if send_serial_break(&port.path).is_ok() {
                if let Some(device) = wait_for_msu2(cli, wait).await {
                    println!(
                        "reset complete: {} (firmware {})",
                        device.path(),
                        device.version_string()
                    );
                    return Ok(());
                }
            }

            println!("Trying DTR/RTS reset on {}...", port.path);
            if pulse_control_lines(&port.path, cli.baud).await.is_ok() {
                if let Some(device) = wait_for_msu2(cli, wait).await {
                    println!(
                        "reset complete: {} (firmware {})",
                        device.path(),
                        device.version_string()
                    );
                    return Ok(());
                }
            }
        }
    }

    // 2. 兜底：通过 hub 端口断电。部分 hub 报告 ppps 但并不会真正切 VBUS。
    let (hub, hub_port) = match (hub, hub_port) {
        (Some(hub), Some(port)) => (hub.to_owned(), port),
        _ => find_msu2_hub_port()?,
    };

    println!("Power-cycling MSU2 on hub {hub} port {hub_port}...");
    uhubctl_set(&hub, hub_port, "off")?;
    tokio::time::sleep(Duration::from_millis(2000)).await;

    let still_present = msu2_core::list_ports()
        .map(|ports| ports.iter().any(|port| port.is_msu2()))
        .unwrap_or(false);

    uhubctl_set(&hub, hub_port, "on")?;

    if still_present {
        eprintln!(
            "warning: MSU2 still enumerated while hub port {hub_port} was off; \
             this hub did not actually cut VBUS, so the MCU was not power-cycled."
        );
    }

    if let Some(device) = wait_for_msu2(cli, wait).await {
        println!(
            "reset complete: {} (firmware {})",
            device.path(),
            device.version_string()
        );
        return Ok(());
    }

    bail!(
        "MSU2 did not recover within {wait_ms} ms. Serial BREAK/DTR/RTS did not reset it, \
         and the hub port did not really cut VBUS. Use a port that switches VBUS, or \
         power-cycle the USB cable once."
    )
}

fn uhubctl_set(hub: &str, port: u8, action: &str) -> anyhow::Result<()> {
    let port_arg = port.to_string();
    let output = ProcessCommand::new("uhubctl")
        .args(["-S", "-l", hub, "-p", port_arg.as_str(), "-a", action])
        .output()
        .context("failed to run uhubctl; install uhubctl or pass --hub/--hub-port")?;

    if !output.status.success() {
        bail!(
            "uhubctl failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(())
}

async fn wait_for_msu2(cli: &Cli, timeout: Duration) -> Option<Msu2Device> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Ok(ports) = msu2_core::list_ports() {
            if let Some(port) = ports.into_iter().find(|port| port.is_msu2()) {
                if let Ok(device) = Msu2Device::connect(
                    port.path.clone(),
                    cli.baud,
                    Duration::from_millis(cli.timeout_ms),
                )
                .await
                {
                    return Some(device);
                }
            }
        }

        if Instant::now() >= deadline {
            return None;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

async fn pulse_control_lines(path: &str, baud: u32) -> anyhow::Result<()> {
    use tokio_serial::{SerialPort, SerialPortBuilderExt};

    let mut stream = tokio_serial::new(path, baud)
        .open_native_async()
        .with_context(|| format!("failed to open {path} for DTR/RTS reset"))?;

    stream.write_data_terminal_ready(false)?;
    stream.write_request_to_send(false)?;
    tokio::time::sleep(Duration::from_millis(200)).await;
    stream.write_data_terminal_ready(true)?;
    tokio::time::sleep(Duration::from_millis(200)).await;
    stream.write_data_terminal_ready(false)?;
    tokio::time::sleep(Duration::from_millis(200)).await;
    stream.write_data_terminal_ready(true)?;
    Ok(())
}

fn find_msu2_hub_port() -> anyhow::Result<(String, u8)> {
    let output = ProcessCommand::new("uhubctl")
        .arg("-S")
        .output()
        .context("failed to run uhubctl; install uhubctl or pass --hub/--hub-port")?;

    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&output.stderr));

    let mut current_hub: Option<String> = None;
    for line in text.lines() {
        let trimmed = line.trim();

        if let Some(rest) = trimmed.strip_prefix("Current status for hub ") {
            if let Some(hub) = rest.split_whitespace().next() {
                current_hub = Some(hub.to_owned());
            }
        }

        if trimmed.starts_with("Port ") && trimmed.contains("1a86:fe0c") {
            let hub = current_hub
                .clone()
                .context("found MSU2 port but could not determine its hub")?;
            let port_str = trimmed
                .strip_prefix("Port ")
                .and_then(|rest| rest.split(':').next())
                .unwrap_or("");
            let port = port_str
                .parse::<u8>()
                .with_context(|| format!("invalid hub port number: {port_str}"))?;
            return Ok((hub, port));
        }
    }

    bail!("MSU2 (1a86:fe0c) not found in uhubctl output; pass --hub and --hub-port")
}

async fn cmd_raw(cli: &Cli, hex: &str, handshake: bool, delay_ms: u64) -> anyhow::Result<()> {
    let path = resolve_port(cli)?;
    let timeout = Duration::from_millis(cli.timeout_ms);
    let mut device = if handshake {
        Msu2Device::connect(path.clone(), cli.baud, timeout)
            .await
            .with_context(|| format!("failed to connect to {path}"))?
    } else {
        Msu2Device::open(path.clone(), cli.baud, timeout)
            .with_context(|| format!("failed to open {path}"))?
    };

    let bytes = parse_hex_bytes(hex)?;
    if delay_ms > 0 {
        tokio::time::sleep(Duration::from_millis(delay_ms)).await;
    }
    device
        .send_raw(&bytes)
        .await
        .context("failed to write bytes")?;
    println!("TX: {}", hex_bytes(&bytes));

    let deadline = Instant::now() + Duration::from_millis(cli.timeout_ms);
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            println!("RX: <timeout; this command may be write-only>");
            break;
        }

        match device.next_frame(remaining).await {
            Ok(Frame::Heartbeat { .. }) => continue,
            Ok(frame) => {
                println!("RX: {frame}");
                break;
            }
            Err(msu2_core::Error::Timeout) => {
                println!("RX: <timeout; this command may be write-only>");
                break;
            }
            Err(error) => return Err(error.into()),
        }
    }

    Ok(())
}

#[cfg(unix)]
fn send_serial_break(path: &str) -> anyhow::Result<()> {
    use std::os::unix::io::AsRawFd;

    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .with_context(|| format!("failed to open {path} for BREAK"))?;

    let result = unsafe { libc::tcsendbreak(file.as_raw_fd(), 0) };
    if result != 0 {
        bail!("tcsendbreak failed: {}", std::io::Error::last_os_error());
    }
    std::thread::sleep(Duration::from_millis(500));
    Ok(())
}

#[cfg(not(unix))]
fn send_serial_break(_path: &str) -> anyhow::Result<()> {
    bail!("serial BREAK reset is only implemented on Unix")
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

fn parse_hex_bytes(input: &str) -> anyhow::Result<Vec<u8>> {
    let cleaned: String = input
        .chars()
        .filter(|ch| !ch.is_whitespace() && *ch != '_' && *ch != ':' && *ch != '-')
        .collect();
    let cleaned = cleaned
        .strip_prefix("0x")
        .or_else(|| cleaned.strip_prefix("0X"))
        .unwrap_or(&cleaned);

    if cleaned.is_empty() || !cleaned.chars().all(|ch| ch.is_ascii_hexdigit()) {
        bail!("invalid hex string");
    }
    if cleaned.len() % 2 != 0 {
        bail!("hex string must contain an even number of digits");
    }

    let mut bytes = Vec::with_capacity(cleaned.len() / 2);
    for index in (0..cleaned.len()).step_by(2) {
        bytes.push(
            u8::from_str_radix(&cleaned[index..index + 2], 16)
                .with_context(|| format!("invalid hex byte at offset {index}"))?,
        );
    }
    Ok(bytes)
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
