//! MSU2 USB 副屏协议与传输层。
//!
//! 设备通过 USB CDC 虚拟串口通信，主机发送 6 字节定长命令。
//! 模块声明统一使用 `crate_macro`。

crate_macro::mod_pub!(button, device, display, error, protocol, sfr, transport);

pub use button::{ButtonConfig, ButtonState, Gesture};
pub use device::{Msu2Device, STREAM_TIMEOUT};
pub use error::Error;
pub use protocol::Frame;
pub use sfr::{SfrEntry, SfrKind, SfrTable, SfrValue};
pub use transport::{PortInfo, SerialTransport, Transport, find_msu2_port, list_ports};
