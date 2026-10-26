//! Matter BTP over the unit's DA14580 peripheral stack and UART GTL transport.
#[cfg(any(target_os = "linux", test))]
mod gtl;
#[cfg(target_os = "linux")]
mod uart;

use std::sync::atomic::{AtomicBool, Ordering};
#[cfg(target_os = "linux")]
use std::time::{Duration, Instant};
use rs_matter::transport::network::btp::Btp;
#[cfg(target_os = "linux")]
use rs_matter::transport::network::{btp::AdvData, BtAddr};
use rs_matter::{BasicCommData, Matter};
#[cfg(target_os = "linux")]
use rs_matter::sc::pase::CommWindowState;

static CONNECTED: AtomicBool = AtomicBool::new(false);
static READY: AtomicBool = AtomicBool::new(false);

pub fn sleep_blocked() -> bool { CONNECTED.load(Ordering::SeqCst) }
pub fn ready() -> bool { READY.load(Ordering::SeqCst) }

pub async fn run(matter: &Matter<'_>, comm: &BasicCommData, btp: &Btp, serial: &str) {
    #[cfg(target_os = "linux")]
    {
        if let Err(e) = serve(matter, comm, btp, serial).await {
            crate::logf!("matter BLE: unavailable: {e}; IP commissioning remains available");
        }
        CONNECTED.store(false, Ordering::SeqCst);
        READY.store(false, Ordering::SeqCst);
        btp.reset();
    }
    #[cfg(not(target_os = "linux"))]
    let _ = (matter, comm, btp, serial);
    std::future::pending::<()>().await;
}

#[cfg(target_os = "linux")]
async fn serve(matter: &Matter<'_>, comm: &BasicCommData, btp: &Btp, serial: &str) -> std::io::Result<()> {
    use std::io::{Error, ErrorKind};
    use embassy_futures::select::{select, Either};
    let mut uart = uart::Uart::open()?;
    let info = rs_matter::dm::clusters::basic_info::BasicInfoConfig {
        vid: rs_matter::dm::devices::test::TEST_VID,
        pid: rs_matter::dm::devices::test::TEST_PID,
        ..rs_matter::dm::clusters::basic_info::BasicInfoConfig::new()
    };
    let service: Vec<u8> = AdvData::new(&info, comm.discriminator).service_iter().collect();
    uart.command(2, &gtl::configuration(&service, serial)?, 4, &[1, 1]).await?;
    READY.store(true, Ordering::SeqCst);
    crate::logf!("matter BLE: DA14580 peripheral ready");
    let mut advertising = false;
    let mut peer = None;
    let mut mtu = 23;
    let mut pending = None;
    let mut closed_since = None;
    let mut packet = [0; gtl::MAX_VALUE];
    loop {
        let open = !matches!(matter.comm_window_state(), CommWindowState::Closed);
        if open { closed_since = None; }
        else if peer.is_some() {
            closed_since.get_or_insert_with(Instant::now);
        }
        if peer.is_some() && (btp.timeout()
            || closed_since.is_some_and(|at| at.elapsed() >= Duration::from_secs(30))) {
            uart.command(10, &[], 12, &[1]).await?;
            advertising = false;
            peer = None;
            pending = None;
            CONNECTED.store(false, Ordering::SeqCst);
            btp.reset();
        }
        if peer.is_none() && open != advertising {
            uart.command(if open { 9 } else { 10 }, &[], if open { 11 } else { 12 }, &[1]).await?;
            advertising = open;
        }
        if pending.is_some_and(|at: Instant| at.elapsed() > Duration::from_secs(5)) {
            return Err(Error::new(ErrorKind::TimedOut, "BLE indication confirmation timed out"));
        }
        if peer.is_some() && pending.is_none() {
            let len = btp.process_outgoing(Some(mtu), &mut packet)
                .map_err(|e| Error::new(ErrorKind::InvalidData, format!("BTP output: {e:?}")))?;
            if len != 0 {
                let mut body = vec![2, 1];
                body.extend_from_slice(&(len as u16).to_le_bytes());
                body.extend_from_slice(&packet[..len]);
                uart.send(0x20, &body).await?;
                pending = Some(Instant::now());
            }
        }
        // The tick checks commissioning changes and BTP acknowledgement deadlines.
        let message = match select(uart.receive(), async_io::Timer::after(Duration::from_millis(50))).await {
            Either::First(result) => Some(result?),
            Either::Second(_) => None,
        };
        let Some((op, body)) = message else { continue; };
        match (op, body.as_slice()) {
            (0x15, [addr @ .., lo, hi, 1, _]) if addr.len() == 6 => {
                btp.reset();
                peer = Some(BtAddr(addr.try_into().unwrap()));
                mtu = u16::from_le_bytes([*lo, *hi]).clamp(23, 131);
                CONNECTED.store(true, Ordering::SeqCst);
                crate::logf!("matter BLE: connected, MTU {mtu}");
            }
            (0x16, _) => {
                peer = None;
                pending = None;
                // The peripheral firmware resumes advertising after disconnect.
                advertising = true;
                CONNECTED.store(false, Ordering::SeqCst);
                btp.reset();
            }
            // The profile always sends its 128-byte buffer, with a separate value length.
            (0x22, [2, 0, lo, hi, data @ ..]) => {
                if let Some(peer) = peer {
                    let len = u16::from_le_bytes([*lo, *hi]) as usize;
                    if data.len() != gtl::MAX_VALUE || len == 0 || len > data.len() {
                        return Err(Error::new(ErrorKind::InvalidData, "invalid C1 write length"));
                    }
                    if let Err(e) = btp.process_incoming(Some(mtu), peer, &data[..len]) {
                        crate::logf!("matter BLE: rejected BTP write: {e:?}");
                    }
                }
            }
            // 0x21 reports request errors; 0x24 confirms delivery to the central.
            (0x21, [2, 1, _, _, status]) => {
                if *status != 0 { return Err(Error::other(format!("BLE indication failed: {status}"))); }
            }
            (0x24, [2, 1, _, _, 1]) => pending = None,
            _ => {}
        }
    }
}
