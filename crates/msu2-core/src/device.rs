//! 面向设备的异步 API：握手、SFR 读取、ADC、Flash 与 LCD 操作。

use std::time::{Duration, Instant};

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

    /// 读外部 Flash 的一个字节。
    pub async fn read_flash_byte(&mut self, address: u32) -> Result<u8, Error> {
        Ok(self.request(protocol::flash_read_byte(address)).await?[5])
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

    /// 发送原始字节，不等待响应。
    pub async fn send_raw(&mut self, bytes: &[u8]) -> Result<(), Error> {
        self.transport.send(bytes).await
    }

    /// 读取下一帧（用于 `raw` 命令调试）。
    pub async fn next_frame(&mut self, timeout: Duration) -> Result<Frame, Error> {
        self.transport.next_frame(timeout).await
    }

    /// 用纯色填充矩形。
    ///
    /// 显式预载每个 256 字节块的 64 个 4 字节字，再调用 `02 03 08` 刷出，
    /// 避免依赖 `02 04` 填充字命令。该操作只写不读，适合作为显示链路的冒烟测试。
    pub async fn fill_rect(
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

        // 固件刚完成握手时 LCD/命令状态尚未稳定，先留出一段稳定期。
        #[cfg(not(test))]
        {
            tokio::time::sleep(Duration::from_millis(1500)).await;
        }

        let total_bytes = u32::from(width) * u32::from(height) * 2;
        let chunks = total_bytes.div_ceil(256) as usize;
        let [color_hi, color_lo] = color.to_be_bytes();
        let color_word = [color_hi, color_lo, color_hi, color_lo];

        // 参考实现会逐条发送窗口设置，并在 LCD_ADD 后等待设备响应。
        // 这里分开发送并留出建立时间，避免固件尚未准备好就开始 DMA。
        self.transport.send(&protocol::lcd_set_xy(x, y)).await?;
        #[cfg(not(test))]
        tokio::time::sleep(Duration::from_millis(2)).await;
        self.transport
            .send(&protocol::lcd_set_size(width, height))
            .await?;
        #[cfg(not(test))]
        tokio::time::sleep(Duration::from_millis(2)).await;
        self.transport.send(&protocol::lcd_stream_start()).await?;
        // 参考实现会在 LCD_ADD 后等待任意一帧（通常是心跳），再开始发数据。
        #[cfg(not(test))]
        {
            let _ = self.transport.next_frame(Duration::from_millis(1000)).await;
        }

        let mut packet = Vec::with_capacity(chunks * (64 * 6 + 6) + 6);
        let mut remaining = total_bytes;
        while remaining > 0 {
            let chunk = remaining.min(256) as u16;
            for index in 0..64u8 {
                packet.extend_from_slice(&[
                    protocol::CMD_DATA,
                    index,
                    color_word[0],
                    color_word[1],
                    color_word[2],
                    color_word[3],
                ]);
            }
            packet.extend_from_slice(&protocol::lcd_flush_chunk(chunk));
            remaining -= u32::from(chunk);
        }

        packet.extend_from_slice(&protocol::lcd_sync());
        self.transport.send(&packet).await
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

    /// 用内置 16×16 GB2312 字库显示一个字符（传入 GB2312 双字节编码）。
    pub async fn display_gb2312(
        &mut self,
        x: u16,
        y: u16,
        gb_hi: u8,
        gb_lo: u8,
        fg: u16,
        bg: u16,
    ) -> Result<(), Error> {
        self.transport.send(&protocol::lcd_set_xy(x, y)).await?;
        self.transport
            .send(&protocol::lcd_set_color(fg, bg))
            .await?;
        self.transport
            .send(&protocol::lcd_gb2312_16x16(gb_hi, gb_lo))
            .await?;
        let _ = self.transport.next_frame(Duration::from_millis(1000)).await;
        Ok(())
    }

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
mod tests {
    use std::collections::VecDeque;

    use super::*;

    struct MockTransport {
        frames: VecDeque<Frame>,
        sent: Vec<Vec<u8>>,
    }

    impl MockTransport {
        fn new(frames: impl IntoIterator<Item = Frame>) -> Self {
            Self {
                frames: frames.into_iter().collect(),
                sent: Vec::new(),
            }
        }
    }

    impl Transport for MockTransport {
        async fn send(&mut self, bytes: &[u8]) -> Result<(), Error> {
            self.sent.push(bytes.to_vec());
            Ok(())
        }

        async fn next_frame(&mut self, _timeout: Duration) -> Result<Frame, Error> {
            self.frames.pop_front().ok_or(Error::Timeout)
        }
    }

    fn mock_device(frames: impl IntoIterator<Item = Frame>) -> Msu2Device<MockTransport> {
        Msu2Device::from_transport(
            MockTransport::new(frames),
            "mock",
            Duration::from_millis(100),
        )
    }

    #[tokio::test]
    async fn handshake_waits_for_heartbeat_then_ack() {
        let mut device = mock_device([
            Frame::Heartbeat { version: *b"01" },
            Frame::Heartbeat { version: *b"01" },
            Frame::Handshake,
        ]);

        let version = device
            .handshake(Duration::from_millis(100))
            .await
            .expect("handshake should succeed");

        assert_eq!(version, *b"01");
        assert_eq!(device.version_string(), "01");
        assert_eq!(device.transport.sent, vec![protocol::HANDSHAKE.to_vec()]);
    }

    #[tokio::test]
    async fn request_skips_heartbeats() {
        let mut device = mock_device([
            Frame::Heartbeat { version: *b"01" },
            Frame::Response([0x00, b'0', 0x00, 0x01, 0x00, 0xAB]),
        ]);

        assert_eq!(device.read_sfr_u8(0x0100).await.unwrap(), 0xAB);
        assert_eq!(
            device.transport.sent,
            vec![protocol::sfr_read_u8(0x0100).to_vec()]
        );
    }

    #[tokio::test]
    async fn fill_rect_words_uses_fill_word_frames() {
        let mut device = mock_device([]);
        device.fill_rect_words(0, 0, 2, 1, 0xF800).await.unwrap();

        assert_eq!(device.transport.sent.len(), 4);
        assert_eq!(device.transport.sent[0], protocol::lcd_set_xy(0, 0));
        assert_eq!(device.transport.sent[1], protocol::lcd_set_size(2, 1));
        assert_eq!(device.transport.sent[2], protocol::lcd_stream_start());
        assert_eq!(
            device.transport.sent[3],
            [protocol::lcd_set_fill(0xF800), protocol::lcd_flush_chunk(4)].concat()
        );
    }

    #[tokio::test]
    async fn fill_rect_batches_stream_commands() {
        let mut device = mock_device([]);
        device.fill_rect(0, 0, 1, 1, 0xF800).await.unwrap();

        assert_eq!(device.transport.sent.len(), 4);
        assert_eq!(device.transport.sent[0], protocol::lcd_set_xy(0, 0));
        assert_eq!(device.transport.sent[1], protocol::lcd_set_size(1, 1));
        assert_eq!(device.transport.sent[2], protocol::lcd_stream_start());

        let packet = &device.transport.sent[3];
        assert_eq!(packet.len(), 64 * 6 + 6 + 6);
        assert_eq!(
            &packet[..6],
            &[protocol::CMD_DATA, 0x00, 0xF8, 0x00, 0xF8, 0x00]
        );
        assert_eq!(&packet[64 * 6..64 * 6 + 6], &protocol::lcd_flush_chunk(2));
        assert_eq!(&packet[packet.len() - 6..], &protocol::lcd_sync());
    }
}
