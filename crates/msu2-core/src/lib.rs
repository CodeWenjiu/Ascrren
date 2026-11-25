//! MSU2 USB 副屏协议与传输层。
//!
//! 设备通过 USB CDC 虚拟串口通信，主机发送 6 字节定长命令。

pub mod button;
pub mod device;
pub mod display;
pub mod error;
pub mod protocol;
pub mod sfr;
pub mod transport;

pub use button::{ButtonConfig, ButtonState, Press};
pub use device::{Msu2Device, STREAM_TIMEOUT};
pub use error::Error;
pub use protocol::Frame;
pub use sfr::{SfrEntry, SfrKind, SfrTable, SfrValue};
pub use transport::{PortInfo, SerialTransport, Transport, find_msu2_port, list_ports};
