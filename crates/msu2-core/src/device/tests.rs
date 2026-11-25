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
