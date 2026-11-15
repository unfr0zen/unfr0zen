//! What the nestlocal service and the thread-rcp bridge share: the units'
//! device tables, the radio's SPI, and its wake line.

pub mod device;
pub mod radio_wakeup;
pub mod spi;
