//! USB CDC 传输层与串口枚举（基于 `tokio-serial` 的异步实现）。

use std::time::{Duration, Instant};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_serial::{SerialPortInfo, SerialPortType, SerialStream};

use crate::error::Error;
use crate::protocol::Frame;

/// 匹配的 MSU2 USB VID。
pub const MSU2_VID: u16 = 0x1a86;
/// 匹配的 MSU2 USB PID。
pub const MSU2_PID: u16 = 0xfe0c;

/// 一个串口及其 USB 元数据。
#[derive(Debug, Clone)]
pub struct PortInfo {
    pub path: String,
    pub vid: Option<u16>,
    pub pid: Option<u16>,
    pub product: Option<String>,
    pub manufacturer: Option<String>,
    pub serial_number: Option<String>,
}

impl PortInfo {
    pub fn is_msu2(&self) -> bool {
        self.vid == Some(MSU2_VID) && self.pid == Some(MSU2_PID)
    }

    pub fn vid_pid(&self) -> String {
        match (self.vid, self.pid) {
            (Some(vid), Some(pid)) => format!("{vid:04x}:{pid:04x}"),
            _ => "-".to_owned(),
        }
    }
}

impl From<SerialPortInfo> for PortInfo {
    fn from(info: SerialPortInfo) -> Self {
        match info.port_type {
            SerialPortType::UsbPort(usb) => Self {
                path: info.port_name,
                vid: Some(usb.vid),
                pid: Some(usb.pid),
                product: usb.product,
                manufacturer: usb.manufacturer,
                serial_number: usb.serial_number,
            },
            _ => Self {
                path: info.port_name,
                vid: None,
                pid: None,
                product: None,
                manufacturer: None,
                serial_number: None,
            },
        }
    }
}

/// 列出系统串口。
pub fn list_ports() -> Result<Vec<PortInfo>, Error> {
    let ports = tokio_serial::available_ports().map_err(Error::Serial)?;
    Ok(ports.into_iter().map(PortInfo::from).collect())
}

/// 查找唯一的 MSU2 串口。
pub fn find_msu2_port() -> Result<PortInfo, Error> {
    let matches: Vec<PortInfo> = list_ports()?
        .into_iter()
        .filter(PortInfo::is_msu2)
        .collect();

    match matches.len() {
        0 => Err(Error::DeviceNotFound),
        1 => matches.into_iter().next().ok_or(Error::DeviceNotFound),
        _ => {
            let paths = matches
                .iter()
                .map(|port| port.path.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            Err(Error::MultipleDevices(paths))
        }
    }
}

/// 异步传输层抽象，便于用 mock 测试协议逻辑。
#[allow(async_fn_in_trait)]
pub trait Transport {
    async fn send(&mut self, bytes: &[u8]) -> Result<(), Error>;
    async fn next_frame(&mut self, timeout: Duration) -> Result<Frame, Error>;
}

/// 基于 `tokio-serial` 的 USB CDC 传输。
pub struct SerialTransport {
    stream: SerialStream,
    port_name: String,
    buffer: Vec<u8>,
}

impl SerialTransport {
    pub fn open(port_name: &str, baud: u32) -> Result<Self, Error> {
        use tokio_serial::SerialPortBuilderExt;

        let stream = tokio_serial::new(port_name, baud)
            .open_native_async()
            .map_err(Error::Serial)?;

        Ok(Self {
            stream,
            port_name: port_name.to_owned(),
            buffer: Vec::with_capacity(512),
        })
    }

    pub fn port_name(&self) -> &str {
        &self.port_name
    }
}

impl Transport for SerialTransport {
    async fn send(&mut self, bytes: &[u8]) -> Result<(), Error> {
        self.stream.write_all(bytes).await?;
        self.stream.flush().await?;
        Ok(())
    }

    async fn next_frame(&mut self, timeout: Duration) -> Result<Frame, Error> {
        let deadline = Instant::now() + timeout;

        loop {
            if self.buffer.len() >= 6 {
                let mut bytes = [0u8; 6];
                bytes.copy_from_slice(&self.buffer[..6]);
                self.buffer.drain(..6);
                return Ok(Frame::from_bytes(bytes));
            }

            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(Error::Timeout);
            }

            let mut chunk = [0u8; 256];
            match tokio::time::timeout(remaining, self.stream.read(&mut chunk)).await {
                Err(_) => return Err(Error::Timeout),
                Ok(Ok(0)) => tokio::time::sleep(Duration::from_millis(1)).await,
                Ok(Ok(n)) => self.buffer.extend_from_slice(&chunk[..n]),
                Ok(Err(error)) => return Err(Error::Io(error)),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_msu2_usb_ids() {
        let port = PortInfo {
            path: "/dev/ttyACM0".to_owned(),
            vid: Some(MSU2_VID),
            pid: Some(MSU2_PID),
            product: Some("CH32x035".to_owned()),
            manufacturer: Some("wch.cn".to_owned()),
            serial_number: None,
        };
        assert!(port.is_msu2());
        assert_eq!(port.vid_pid(), "1a86:fe0c");
    }
}
