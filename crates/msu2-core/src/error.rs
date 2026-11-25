use std::io;

/// MSU2 驱动错误。
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("serial port error: {0}")]
    Serial(#[from] tokio_serial::Error),

    #[error("I/O error: {0}")]
    Io(#[from] io::Error),

    #[error("timed out waiting for the device")]
    Timeout,

    #[error("MSU2 device not found (expected USB VID:PID 1a86:fe0c); use --port to specify a port")]
    DeviceNotFound,

    #[error("multiple MSU2 devices found: {0}")]
    MultipleDevices(String),

    #[error("protocol error: {0}")]
    Protocol(String),

    #[error("failed to parse SFR table: {0}")]
    SfrParse(String),

    #[error("SFR entry not found: {0}")]
    SfrNotFound(String),

    #[error("SFR entry {0} uses an unsupported type")]
    UnsupportedSfr(String),
}
