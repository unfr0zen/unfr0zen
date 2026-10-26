//! Device protocols and recovery logic for unfr0zen, independent of the host's
//! USB and serial stacks: every transport is a trait the front end provides.

pub mod boot_branding;
pub mod bulk;
pub mod console;
mod device;
pub mod diamond3;
pub mod j49;
pub mod nestlocal;
pub mod nlclient;
pub mod patch;
pub mod shell;
mod slot;

pub use device::Device;
pub use slot::Slot;

use std::fmt;

#[derive(Debug)]
pub enum Error {
    Io(String),
    Timeout(String),
    Device(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Error::Io(m) | Error::Timeout(m) | Error::Device(m) => f.write_str(m),
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(error: std::io::Error) -> Self {
        Error::Io(error.to_string())
    }
}

pub type Result<T> = std::result::Result<T, Error>;

/// Fails with a device error.
pub(crate) fn device<T>(message: impl Into<String>) -> Result<T> {
    Err(Error::Device(message.into()))
}
