//! Driver errors.

/// Errors of the driver core.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("USB: {0}")]
    Usb(#[from] dt295_usb::Error),
    /// A transfer the protocol needs failed without a transport error of its own (a
    /// short control transfer, or a failed exchange; status code
    /// `DT295_E_USB`).
    #[error("USB transfer: {0}")]
    Transfer(String),
    #[error("timeout: {0}")]
    Timeout(&'static str),
    #[error("protocol: {0}")]
    Protocol(String),
    #[error("firmware: {0}")]
    Firmware(String),
    #[error("invalid parameter: {0}")]
    Param(&'static str),
    #[error("device not found")]
    NoDevice,
    #[error("wrong device state: {0}")]
    State(String),
    #[error("I/O: {0}")]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, Error>;

impl Error {
    /// The numeric status code of this error (`DT295_E_*`), for the log lines that
    /// print it (`rc=%d`).
    pub fn code(&self) -> i32 {
        match self {
            Error::Usb(_) | Error::Transfer(_) => -1,
            Error::Timeout(_) => -2,
            Error::Protocol(_) => -3,
            Error::Param(_) => -4,
            // Unreadable files count as DT295_E_FW.
            Error::Firmware(_) | Error::Io(_) => -5,
            Error::NoDevice => -7,
            Error::State(_) => -8,
        }
    }

    /// The error as `DT295_E_USB`: a failed sub-step reported as a failure of the
    /// step's transfer.
    pub fn into_usb(self) -> Error {
        match self {
            e @ (Error::Usb(_) | Error::Transfer(_)) => e,
            other => Error::Transfer(other.to_string()),
        }
    }
}
