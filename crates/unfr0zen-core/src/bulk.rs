//! A USB bulk pipe, which each device's protocols speak over.

use std::time::Duration;

use crate::Result;

/// A bulk OUT and IN endpoint pair.
pub trait Bulk {
    fn send(&mut self, data: &[u8], timeout: Duration) -> Result<()>;
    /// One IN transfer of up to `max` bytes; it ends early at a short packet.
    fn receive(&mut self, max: usize, timeout: Duration) -> Result<Vec<u8>>;
}
