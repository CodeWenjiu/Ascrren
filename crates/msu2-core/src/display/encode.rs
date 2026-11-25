//! RGB565 framebuffer 的流式编码。
//!
//! 编码遵循厂商 Demo 的实际发送顺序：每个 256 字节页先发 `02 04`
//! 填充字，只对不同的 word 发 `04`，再 flush。

use super::*;

/// 编码一个 tile 的流式发送序列。
///
/// 序列为：设置窗口 → 开始流式 → 厂商式填充字/差分数据 → flush。
pub fn encode_tile_stream(frame: &[u16], frame_width: u16, tile: Rect) -> Result<Vec<u8>, Error> {
    if tile.width == 0 || tile.height == 0 {
        return Err(Error::Protocol("tile must be non-empty".to_owned()));
    }
    if tile.right() > frame_width {
        return Err(Error::Protocol("tile exceeds framebuffer width".to_owned()));
    }
    let rows = frame.len() / usize::from(frame_width);
    if tile.bottom() > rows as u16 {
        return Err(Error::Protocol(
            "tile exceeds framebuffer height".to_owned(),
        ));
    }

    let mut packet = Vec::new();
    packet.extend_from_slice(&protocol::lcd_set_xy(tile.x, tile.y));
    packet.extend_from_slice(&protocol::lcd_set_size(tile.width, tile.height));
    packet.extend_from_slice(&protocol::lcd_stream_start());

    let total_bytes = usize::from(tile.width) * usize::from(tile.height) * 2;
    let mut bytes = Vec::with_capacity(total_bytes);
    for y in tile.y..tile.bottom() {
        for x in tile.x..tile.right() {
            let pixel = frame[usize::from(y) * usize::from(frame_width) + usize::from(x)];
            bytes.extend_from_slice(&pixel.to_be_bytes());
        }
    }

    packet.extend_from_slice(&encode_rgb565_frame(&bytes, tile.width, tile.height)?);
    Ok(packet)
}

/// 把 RGB565 framebuffer 转成协议使用的大端字节流。
pub fn rgb565_bytes(frame: &[u16]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(frame.len() * 2);
    for pixel in frame {
        bytes.extend_from_slice(&pixel.to_be_bytes());
    }
    bytes
}

/// 把多个矩形编码成一个连续 packet，每个矩形仍保留自己的窗口设置和
/// `stream_start`。多个矩形合并成一次 USB 写入可显著减少传输次数。
pub fn encode_tiles(frame: &[u16], frame_width: u16, tiles: &[Rect]) -> Result<Vec<u8>, Error> {
    let mut packet = Vec::new();
    for tile in tiles {
        packet.extend_from_slice(&encode_tile_stream(frame, frame_width, *tile)?);
    }
    Ok(packet)
}

/// 编码一整帧 RGB565 数据，匹配厂商 Demo 的实际发送顺序。
///
/// 对每个完整的 256 字节页：先发 `02 04` 填充字，只对与填充字不同的
/// 4 字节 word 发 `04`，最后发 `02 03 08` flush。最后不足 256 字节的页
/// 按厂商实现发送全部 64 个数据帧，不使用填充字。
///
/// 返回的数据不包含 `set_xy` / `set_size` / `stream_start` 前缀。
pub fn encode_rgb565_frame(frame: &[u8], width: u16, height: u16) -> Result<Vec<u8>, Error> {
    let expected = usize::from(width) * usize::from(height) * 2;
    if frame.len() != expected {
        return Err(Error::Protocol(format!(
            "frame length mismatch: expected {expected}, got {}",
            frame.len()
        )));
    }

    let mut packet = Vec::new();
    let mut offset = 0usize;
    while offset + 256 <= frame.len() {
        let page = &frame[offset..offset + 256];
        let mut words: Vec<([u8; 4], usize)> = Vec::with_capacity(64);
        for (index, word) in page.chunks_exact(4).enumerate() {
            let word = [word[0], word[1], word[2], word[3]];
            if let Some((_, count)) = words.iter_mut().find(|(seen, _)| *seen == word) {
                *count += 1;
            } else {
                words.push((word, 1));
            }
            debug_assert!(index < 64);
        }

        let fill = words
            .into_iter()
            .max_by_key(|(_, count)| *count)
            .map(|(word, _)| word)
            .expect("a full page has words");
        packet.extend_from_slice(&protocol::lcd_set_fill_word(fill));

        for (index, word) in page.chunks_exact(4).enumerate() {
            let word = [word[0], word[1], word[2], word[3]];
            if word != fill {
                packet.extend_from_slice(&[
                    protocol::CMD_DATA,
                    index as u8,
                    word[0],
                    word[1],
                    word[2],
                    word[3],
                ]);
            }
        }
        packet.extend_from_slice(&protocol::lcd_flush_chunk(256));
        offset += 256;
    }

    if offset < frame.len() {
        let mut page = frame[offset..].to_vec();
        let remaining = page.len();
        page.resize(256, 0xFF);
        for (index, word) in page.chunks_exact(4).enumerate() {
            packet.extend_from_slice(&[
                protocol::CMD_DATA,
                index as u8,
                word[0],
                word[1],
                word[2],
                word[3],
            ]);
        }
        packet.extend_from_slice(&protocol::lcd_flush_chunk(remaining as u16));
    }

    Ok(packet)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_a_16x16_tile() {
        let frame = vec![0x1234u16; 160 * 80];
        let tile = Rect {
            x: 16,
            y: 16,
            width: 16,
            height: 16,
        };
        let packet = encode_tile_stream(&frame, 160, tile).unwrap();

        // Two 16x16 pages, each a fill word + flush, plus the three setup commands.
        assert_eq!(packet.len(), 3 * 6 + 2 * 12);
        assert_eq!(&packet[..6], &protocol::lcd_set_xy(16, 16));
        assert_eq!(&packet[6..12], &protocol::lcd_set_size(16, 16));
        assert_eq!(&packet[12..18], &protocol::lcd_stream_start());
        assert_eq!(
            &packet[18..24],
            &protocol::lcd_set_fill_word([0x12, 0x34, 0x12, 0x34])
        );
        assert_eq!(&packet[24..30], &protocol::lcd_flush_chunk(256));
    }

    #[test]
    fn encodes_rgb565_pages_with_fill_word() {
        let frame = (0..160 * 80 / 2)
            .flat_map(|_| [0x12, 0x34, 0x12, 0x34])
            .collect::<Vec<_>>();
        let packet = encode_rgb565_frame(&frame, 160, 80).unwrap();

        // 100 pages, each fill word + flush, no data frames.
        assert_eq!(packet.len(), 100 * 12);
        assert_eq!(
            &packet[..6],
            &protocol::lcd_set_fill_word([0x12, 0x34, 0x12, 0x34])
        );
        assert_eq!(&packet[6..12], &protocol::lcd_flush_chunk(256));
    }

    #[test]
    fn encodes_rgb565_differences_as_data_frames() {
        let mut frame = vec![0u8; 16 * 16 * 2];
        for word in frame.chunks_exact_mut(4) {
            word.copy_from_slice(&[0x12, 0x34, 0x12, 0x34]);
        }
        frame[0..4].copy_from_slice(&[0xAB, 0xCD, 0xAB, 0xCD]);

        let packet = encode_rgb565_frame(&frame, 16, 16).unwrap();
        let first_page_end = 18;
        assert_eq!(packet.len(), 30);
        assert_eq!(
            &packet[..6],
            &protocol::lcd_set_fill_word([0x12, 0x34, 0x12, 0x34])
        );
        assert_eq!(
            &packet[6..12],
            &[protocol::CMD_DATA, 0, 0xAB, 0xCD, 0xAB, 0xCD]
        );
        assert_eq!(&packet[12..18], &protocol::lcd_flush_chunk(256));
        assert_eq!(
            &packet[first_page_end..first_page_end + 6],
            &protocol::lcd_set_fill_word([0x12, 0x34, 0x12, 0x34])
        );
    }

    #[test]
    fn rejects_out_of_bounds_tiles() {
        let frame = vec![0u16; 160 * 80];
        let err = encode_tile_stream(
            &frame,
            160,
            Rect {
                x: 150,
                y: 0,
                width: 16,
                height: 16,
            },
        )
        .unwrap_err();
        assert!(matches!(err, Error::Protocol(_)));
    }
}
