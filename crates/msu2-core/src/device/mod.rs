//! 面向设备的异步 API：握手、SFR 读取、ADC、Flash 与 LCD 操作。

use std::time::{Duration, Instant};

use crate::display::{Rect, encode_rgb565_frame, encode_tiles};
use crate::error::Error;
use crate::protocol::{self, Frame};
use crate::sfr::{SfrKind, SfrTable, SfrValue};
use crate::transport::{SerialTransport, Transport};

/// 一个已打开的 MSU2 设备。
pub struct Msu2Device<T: Transport = SerialTransport> {
    transport: T,
    path: String,
    version: Option<[u8; 2]>,
    command_timeout: Duration,
}

impl Msu2Device<SerialTransport> {
    /// 打开串口，不执行握手。
    pub fn open(
        path: impl Into<String>,
        baud: u32,
        command_timeout: Duration,
    ) -> Result<Self, Error> {
        let path = path.into();
        let transport = SerialTransport::open(&path, baud)?;
        Ok(Self::from_transport(transport, path, command_timeout))
    }

    /// 打开串口并完成心跳/握手。
    pub async fn connect(
        path: impl Into<String>,
        baud: u32,
        timeout: Duration,
    ) -> Result<Self, Error> {
        let mut device = Self::open(path, baud, timeout)?;
        device.handshake(timeout).await?;
        Ok(device)
    }

    /// 自动查找唯一的 MSU2 设备并连接。
    pub async fn auto_connect(baud: u32, timeout: Duration) -> Result<Self, Error> {
        let port = crate::transport::find_msu2_port()?;
        Self::connect(port.path, baud, timeout).await
    }
}

/// 单次 LCD 流式写入的建议超时。
///
/// 固件卡死时 USB 写会因流控永久阻塞，调用方应当用它包住
/// [`Msu2Device::stream_tiles`] / [`Msu2Device::display_frame`]。
pub const STREAM_TIMEOUT: Duration = Duration::from_secs(3);

impl<T: Transport> Msu2Device<T> {
    pub fn from_transport(
        transport: T,
        path: impl Into<String>,
        command_timeout: Duration,
    ) -> Self {
        Self {
            transport,
            path: path.into(),
            version: None,
            command_timeout,
        }
    }

    /// 等待设备心跳，然后发送 `MSNCN` 握手并等待确认。
    ///
    /// 固件在部分状态下会停止主动心跳，但仍响应 `MSNCN`；因此先发握手，
    /// 不强制要求先收到心跳。
    pub async fn handshake(&mut self, timeout: Duration) -> Result<[u8; 2], Error> {
        let deadline = Instant::now() + timeout;
        let mut version: Option<[u8; 2]> = None;

        self.transport.send(&protocol::HANDSHAKE).await?;

        loop {
            let remaining = remaining_until(deadline)?;
            match self.transport.next_frame(remaining).await? {
                Frame::Handshake => {
                    let version = version.unwrap_or([0, 0]);
                    self.version = Some(version);
                    return Ok(version);
                }
                Frame::Heartbeat { version: seen } => version = Some(seen),
                Frame::Response(_) => {}
            }
        }
    }

    async fn request(&mut self, command: [u8; 6]) -> Result<[u8; 6], Error> {
        self.transport.send(&command).await?;
        let deadline = Instant::now() + self.command_timeout;

        loop {
            let remaining = remaining_until(deadline)?;
            match self.transport.next_frame(remaining).await? {
                Frame::Response(bytes) => return Ok(bytes),
                Frame::Heartbeat { .. } | Frame::Handshake => {}
            }
        }
    }

    /// 读 8 位 SFR，返回响应帧的最后 1 字节。
    pub async fn read_sfr_u8(&mut self, address: u16) -> Result<u8, Error> {
        Ok(self.request(protocol::sfr_read_u8(address)).await?[5])
    }

    /// 读 16 位 SFR，返回响应帧的最后 2 字节（大端）。
    pub async fn read_sfr_u16(&mut self, address: u8) -> Result<u16, Error> {
        let response = self.request(protocol::sfr_read_u16(address)).await?;
        Ok(u16::from_be_bytes([response[4], response[5]]))
    }

    /// 读 ADC 通道，返回 16 位值。
    pub async fn read_adc(&mut self, channel: u8) -> Result<u16, Error> {
        let response = self.request(protocol::adc_read(channel)).await?;
        Ok(u16::from_be_bytes([response[4], response[5]]))
    }

    /// 读一次按键 ADC 并丢弃结果，用于空闲时保持连接。
    ///
    /// 固件在主机长时间不通信后会回到出场动画，长驻程序应当周期性调用。
    pub async fn keepalive(&mut self) -> Result<(), Error> {
        self.read_adc(9).await.map(|_| ())
    }

    /// 连续读取一段 SFR 字节。
    pub async fn read_sfr_block(&mut self, start: u16, len: usize) -> Result<Vec<u8>, Error> {
        let mut data = Vec::with_capacity(len);
        for offset in 0..len {
            let address = start
                .checked_add(offset as u16)
                .ok_or_else(|| Error::Protocol("SFR address overflow".to_owned()))?;
            data.push(self.read_sfr_u8(address).await?);
        }
        Ok(data)
    }

    /// 读取并解析 `0x0100` 处的自描述 SFR 表。
    pub async fn read_sfr_table(&mut self) -> Result<SfrTable, Error> {
        let raw = self.read_sfr_block(0x0100, 256).await?;
        SfrTable::parse(&raw)
    }

    /// 执行厂商 Demo 在 LCD 命令前的初始化读取。
    pub async fn initialize(&mut self) -> Result<SfrTable, Error> {
        let table = self.read_sfr_table().await?;
        let _ = self.read_named(&table, "MSN_Status").await;
        let _ = self.read_named(&table, "MSN_UID").await;
        Ok(table)
    }

    /// 按名称读取 SFR 表中的一个值。
    pub async fn read_named(&mut self, table: &SfrTable, name: &str) -> Result<SfrValue, Error> {
        let entry = table
            .get(name)
            .ok_or_else(|| Error::SfrNotFound(name.to_owned()))?;

        match &entry.kind {
            SfrKind::U8 { address, len } => {
                let mut data = Vec::with_capacity(usize::from(*len));
                for offset in 0..*len {
                    data.push(
                        self.read_sfr_u8(address.wrapping_add(u16::from(offset)))
                            .await?,
                    );
                }
                Ok(SfrValue::Bytes(data))
            }
            SfrKind::U16 { address, len } => {
                if *len <= 1 {
                    Ok(SfrValue::U16(self.read_sfr_u16(*address).await?))
                } else {
                    let mut data = Vec::with_capacity(usize::from(*len) * 2);
                    for offset in 0..*len {
                        let value = self.read_sfr_u16((*address).wrapping_add(offset)).await?;
                        data.extend_from_slice(&value.to_be_bytes());
                    }
                    Ok(SfrValue::Bytes(data))
                }
            }
            SfrKind::U32 { .. } => Err(Error::UnsupportedSfr(name.to_owned())),
            SfrKind::String { data } => {
                Ok(SfrValue::Text(String::from_utf8_lossy(data).into_owned()))
            }
            SfrKind::Array { data } => Ok(SfrValue::Bytes(data.clone())),
        }
    }

    /// 用固件的 `02 04` 填充字路径填充矩形。
    /// 等待 LCD 传输完成（`02 03 09`）。
    ///
    /// 厂商 Demo 在每帧数据末尾都会发这一条命令，让固件把尚未落盘的
    /// 页缓冲推给 LCD。
    pub async fn lcd_sync(&mut self) -> Result<(), Error> {
        self.transport.send(&protocol::lcd_sync()).await
    }

    /// 用固件的 `02 04` 填充字路径填充矩形。
    ///
    /// 这是厂商 Demo `Write_LCD_Screen_fast` 使用的优化路径：每个 256 字节
    /// 块只发送一个 4 字节填充字，不发送 64 个数据帧。仍然属于实验性
    /// streaming 路径。
    pub async fn fill_rect_words(
        &mut self,
        x: u16,
        y: u16,
        width: u16,
        height: u16,
        color: u16,
    ) -> Result<(), Error> {
        if width == 0 || height == 0 {
            return Ok(());
        }

        #[cfg(not(test))]
        tokio::time::sleep(Duration::from_millis(1500)).await;

        self.transport.send(&protocol::lcd_set_xy(x, y)).await?;
        #[cfg(not(test))]
        tokio::time::sleep(Duration::from_millis(2)).await;
        self.transport
            .send(&protocol::lcd_set_size(width, height))
            .await?;
        #[cfg(not(test))]
        tokio::time::sleep(Duration::from_millis(2)).await;
        self.transport.send(&protocol::lcd_stream_start()).await?;
        #[cfg(not(test))]
        {
            let _ = self.transport.next_frame(Duration::from_millis(1000)).await;
        }

        let mut packet = Vec::new();
        let mut remaining = u32::from(width) * u32::from(height) * 2;
        while remaining > 0 {
            let chunk = remaining.min(256) as u16;
            packet.extend_from_slice(&protocol::lcd_set_fill(color));
            packet.extend_from_slice(&protocol::lcd_flush_chunk(chunk));
            remaining -= u32::from(chunk);
        }
        self.transport.send(&packet).await
    }

    /// 增量刷新一组矩形区域。
    ///
    /// 每个矩形都会被编码成独立的窗口设置 + 流式数据 + flush 序列，所有
    /// 序列首尾相接合并成一次 USB 写入。调用方负责只传入真正变化的区域
    /// （例如 `TileGrid::diff` + `TileGrid::merge_tiles` 的结果）。
    ///
    /// 返回总线上实际发送的字节数。
    pub async fn stream_tiles(
        &mut self,
        frame: &[u16],
        frame_width: u16,
        tiles: &[Rect],
    ) -> Result<usize, Error> {
        if tiles.is_empty() {
            return Ok(0);
        }

        let packet = encode_tiles(frame, frame_width, tiles)?;
        let bytes = packet.len();
        self.transport.send(&packet).await?;
        Ok(bytes)
    }

    /// 将一整帧 RGB565 数据流式发送到屏幕。
    ///
    /// 编码遵循厂商 Demo 的实际顺序：每个 256 字节页先发 `02 04`
    /// 填充字，只对不同的 word 发 `04`，再 flush。实验性路径，
    /// 仅在厂商支持的窗口尺寸下使用。
    pub async fn display_frame(
        &mut self,
        x: u16,
        y: u16,
        width: u16,
        height: u16,
        frame: &[u8],
    ) -> Result<(), Error> {
        let packet = encode_rgb565_frame(frame, width, height)?;

        #[cfg(not(test))]
        tokio::time::sleep(Duration::from_millis(1500)).await;

        self.transport.send(&protocol::lcd_set_xy(x, y)).await?;
        #[cfg(not(test))]
        tokio::time::sleep(Duration::from_millis(2)).await;
        self.transport
            .send(&protocol::lcd_set_size(width, height))
            .await?;
        #[cfg(not(test))]
        tokio::time::sleep(Duration::from_millis(2)).await;
        self.transport.send(&protocol::lcd_stream_start()).await?;
        #[cfg(not(test))]
        {
            let _ = self.transport.next_frame(Duration::from_millis(1000)).await;
        }

        self.transport.send(&packet).await
    }

    /// 显示外部 Flash 中预存的彩色图片。
    pub async fn display_flash_image(
        &mut self,
        x: u16,
        y: u16,
        width: u16,
        height: u16,
        page: u16,
    ) -> Result<(), Error> {
        self.transport.send(&protocol::lcd_set_xy(x, y)).await?;
        self.transport
            .send(&protocol::lcd_set_size(width, height))
            .await?;
        self.transport.send(&protocol::lcd_photo(page)).await?;
        let _ = self.transport.next_frame(Duration::from_millis(1000)).await;
        Ok(())
    }

    /// 用内置 32×64 ASCII 字库显示一个字符。
    pub async fn display_ascii(
        &mut self,
        x: u16,
        y: u16,
        ch: u8,
        fg: u16,
        bg: u16,
        font_page: u16,
    ) -> Result<(), Error> {
        self.transport.send(&protocol::lcd_set_xy(x, y)).await?;
        self.transport
            .send(&protocol::lcd_set_color(fg, bg))
            .await?;
        self.transport
            .send(&protocol::lcd_ascii_32x64(ch, font_page))
            .await?;
        let _ = self.transport.next_frame(Duration::from_millis(1000)).await;
        Ok(())
    }

    /// 用内置 32×64 ASCII 字库显示一个字符。
    pub fn path(&self) -> &str {
        &self.path
    }

    pub fn version(&self) -> Option<[u8; 2]> {
        self.version
    }

    pub fn version_string(&self) -> String {
        match self.version {
            Some(version) if version.iter().all(u8::is_ascii_digit) => {
                String::from_utf8_lossy(&version).into_owned()
            }
            _ => "unknown".to_owned(),
        }
    }
}

fn remaining_until(deadline: Instant) -> Result<Duration, Error> {
    let now = Instant::now();
    if now >= deadline {
        Err(Error::Timeout)
    } else {
        Ok(deadline - now)
    }
}

#[cfg(test)]
mod tests;
