use std::path::PathBuf;
use std::time::Duration;

use anyhow::Context;
use clap::{Parser, Subcommand};
use msu2_core::Msu2Device;

mod cmd_display;
mod cmd_info;

use cmd_display::{
    FillSpec, FrameSpec, TextStyle, cmd_ascii, cmd_clear, cmd_fill, cmd_frame, cmd_photo, cmd_text,
    parse_rgb565,
};
use cmd_info::{cmd_info, cmd_key, cmd_ports};

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
