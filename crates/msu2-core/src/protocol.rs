//! MSU2 的 6 字节命令帧与命令构造函数。

use std::fmt;

/// 心跳/控制帧前缀：`00 'M' 'S' 'N'`。
pub const HEARTBEAT_MAGIC: [u8; 4] = [0x00, b'M', b'S', b'N'];

/// 主机握手帧，设备会回同样的 6 字节。
pub const HANDSHAKE: [u8; 6] = [0x00, b'M', b'S', b'N', b'C', b'N'];

pub const CMD_SFR: u8 = 0x00;
pub const SFR_SUBCOMMAND: u8 = b'0';
pub const SFR_READ_U8: u8 = 0x00;
pub const SFR_READ_U16: u8 = 0x20;
pub const SFR_WRITE_U8: u8 = 0x80;
pub const SFR_WRITE_U16: u8 = 0xA0;

pub const CMD_ADC: u8 = 0x08;
pub const CMD_FLASH: u8 = 0x03;
pub const CMD_DATA: u8 = 0x04;
pub const CMD_LCD: u8 = 0x02;

/// 设备发来的 6 字节帧。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Frame {
    /// `00 MSNdd`，`dd` 为两位版本 ASCII。
    Heartbeat { version: [u8; 2] },
    /// `00 MSNCN`。
    Handshake,
    /// 命令响应或其他 6 字节数据。
    Response([u8; 6]),
}

impl Frame {
    pub fn from_bytes(bytes: [u8; 6]) -> Self {
        if bytes[0] == HEARTBEAT_MAGIC[0]
            && bytes[1] == HEARTBEAT_MAGIC[1]
            && bytes[2] == HEARTBEAT_MAGIC[2]
            && bytes[3] == HEARTBEAT_MAGIC[3]
        {
            if bytes[4] == b'C' && bytes[5] == b'N' {
                return Self::Handshake;
            }
            if bytes[4].is_ascii_digit() && bytes[5].is_ascii_digit() {
                return Self::Heartbeat {
                    version: [bytes[4], bytes[5]],
                };
            }
        }
        Self::Response(bytes)
    }

    pub fn bytes(self) -> [u8; 6] {
        match self {
            Self::Heartbeat { version } => [0x00, b'M', b'S', b'N', version[0], version[1]],
            Self::Handshake => HANDSHAKE,
            Self::Response(bytes) => bytes,
        }
    }
}

impl fmt::Display for Frame {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Heartbeat { version } => {
                write!(f, "heartbeat MSN{}", String::from_utf8_lossy(version))
            }
            Self::Handshake => f.write_str("handshake MSNCN"),
            Self::Response(bytes) => {
                f.write_str("response")?;
                for byte in bytes {
                    write!(f, " {byte:02X}")?;
                }
                Ok(())
            }
        }
    }
}

/// 读 8 位 SFR：`00 30 00 addr_hi addr_lo 00`。
pub fn sfr_read_u8(address: u16) -> [u8; 6] {
    [
        CMD_SFR,
        SFR_SUBCOMMAND,
        SFR_READ_U8,
        (address >> 8) as u8,
        address as u8,
        0,
    ]
}

/// 读 16 位 SFR：`00 30 20 addr 00 00`。
pub fn sfr_read_u16(address: u8) -> [u8; 6] {
    [CMD_SFR, SFR_SUBCOMMAND, SFR_READ_U16, address, 0, 0]
}

/// 写 8 位 SFR：`00 30 80 addr_hi addr_lo value`。
pub fn sfr_write_u8(address: u16, value: u8) -> [u8; 6] {
    [
        CMD_SFR,
        SFR_SUBCOMMAND,
        SFR_WRITE_U8,
        (address >> 8) as u8,
        address as u8,
        value,
    ]
}

/// 写 16 位 SFR：`00 30 A0 addr value_hi value_lo`。
pub fn sfr_write_u16(address: u8, value: u16) -> [u8; 6] {
    let [value_hi, value_lo] = value.to_be_bytes();
    [
        CMD_SFR,
        SFR_SUBCOMMAND,
        SFR_WRITE_U16,
        address,
        value_hi,
        value_lo,
    ]
}

/// 读 ADC：`08 channel 00 00 00 00`。
pub fn adc_read(channel: u8) -> [u8; 6] {
    [CMD_ADC, channel, 0, 0, 0, 0]
}

/// 读 Flash 字节：`03 00 addr_hi addr_mid addr_lo 00`。
pub fn flash_read_byte(address: u32) -> [u8; 6] {
    [
        CMD_FLASH,
        0x00,
        (address >> 16) as u8,
        (address >> 8) as u8,
        address as u8,
        0,
    ]
}

/// 设置 LCD 起始坐标：`02 00 x_hi x_lo y_hi y_lo`。
pub fn lcd_set_xy(x: u16, y: u16) -> [u8; 6] {
    [
        CMD_LCD,
        0x00,
        (x >> 8) as u8,
        x as u8,
        (y >> 8) as u8,
        y as u8,
    ]
}

/// 设置 LCD 区域大小：`02 01 w_hi w_lo h_hi h_lo`。
pub fn lcd_set_size(width: u16, height: u16) -> [u8; 6] {
    [
        CMD_LCD,
        0x01,
        (width >> 8) as u8,
        width as u8,
        (height >> 8) as u8,
        height as u8,
    ]
}

/// 设置 LCD 前景色/背景色：`02 02 fc_hi fc_lo bc_hi bc_lo`。
pub fn lcd_set_color(fg: u16, bg: u16) -> [u8; 6] {
    let [fg_hi, fg_lo] = fg.to_be_bytes();
    let [bg_hi, bg_lo] = bg.to_be_bytes();
    [CMD_LCD, 0x02, fg_hi, fg_lo, bg_hi, bg_lo]
}

/// 显示 32×64 ASCII 字符：`02 03 02 char font_page_hi font_page_lo`。
pub fn lcd_ascii_32x64(ch: u8, font_page: u16) -> [u8; 6] {
    [
        CMD_LCD,
        0x03,
        0x02,
        ch,
        (font_page >> 8) as u8,
        font_page as u8,
    ]
}

/// 显示 16×16 GB2312 字符：`02 03 03 gb_hi gb_lo 00`。
pub fn lcd_gb2312_16x16(gb_hi: u8, gb_lo: u8) -> [u8; 6] {
    [CMD_LCD, 0x03, 0x03, gb_hi, gb_lo, 0]
}

/// 显示 Flash 中的彩色图片：`02 03 00 page_hi page_lo 00`。
pub fn lcd_photo(page: u16) -> [u8; 6] {
    [CMD_LCD, 0x03, 0x00, (page >> 8) as u8, page as u8, 0]
}

/// 开始流式写入：`02 03 07 00 00 00`。
pub fn lcd_stream_start() -> [u8; 6] {
    [CMD_LCD, 0x03, 0x07, 0, 0, 0]
}

/// 设置 32 位填充字（两个 RGB565 像素）：`02 04 d0 d1 d2 d3`。
pub fn lcd_set_fill_word(word: [u8; 4]) -> [u8; 6] {
    [CMD_LCD, 0x04, word[0], word[1], word[2], word[3]]
}

/// 设置 32 位填充字（两个相同 RGB565 像素）。
pub fn lcd_set_fill(color: u16) -> [u8; 6] {
    let [hi, lo] = color.to_be_bytes();
    lcd_set_fill_word([hi, lo, hi, lo])
}

/// 刷出一段流式数据：`02 03 08 size_hi size_lo 00`。
pub fn lcd_flush_chunk(size: u16) -> [u8; 6] {
    [CMD_LCD, 0x03, 0x08, (size >> 8) as u8, size as u8, 0]
}

/// 等待 LCD 传输完成：`02 03 09 00 00 00`。
pub fn lcd_sync() -> [u8; 6] {
    [CMD_LCD, 0x03, 0x09, 0, 0, 0]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_control_frames() {
        assert_eq!(
            Frame::from_bytes([0x00, b'M', b'S', b'N', b'0', b'1']),
            Frame::Heartbeat { version: *b"01" }
        );
        assert_eq!(Frame::from_bytes(HANDSHAKE), Frame::Handshake);
        assert_eq!(
            Frame::from_bytes([0x00, b'0', 0x00, 0x01, 0x00, 0xAB]),
            Frame::Response([0x00, b'0', 0x00, 0x01, 0x00, 0xAB])
        );
    }

    #[test]
    fn encodes_sfr_access() {
        assert_eq!(sfr_read_u8(0x0100), [0x00, b'0', 0x00, 0x01, 0x00, 0x00]);
        assert_eq!(sfr_read_u16(0x1F), [0x00, b'0', 0x20, 0x1F, 0x00, 0x00]);
        assert_eq!(
            sfr_write_u16(0x1F, 0x1234),
            [0x00, b'0', 0xA0, 0x1F, 0x12, 0x34]
        );
    }

    #[test]
    fn encodes_fill_word() {
        assert_eq!(lcd_set_fill(0xF800), [0x02, 0x04, 0xF8, 0x00, 0xF8, 0x00]);
        assert_eq!(
            lcd_set_fill_word([0x12, 0x34, 0x56, 0x78]),
            [0x02, 0x04, 0x12, 0x34, 0x56, 0x78]
        );
        assert_eq!(lcd_flush_chunk(256), [0x02, 0x03, 0x08, 0x01, 0x00, 0x00]);
    }
}
