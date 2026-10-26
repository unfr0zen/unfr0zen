//! The unit's clock. nlclient sets it from the cloud's X-nl-service-timestamp
//! (cloud.rs), which nestlocal takes from that same clock: on a unit with no
//! ntpd, nothing else would ever set it (it boots in 2000). So nestlocal sets
//! the clock when it is off, from an NTP server (SNTP, RFC 4330) or, on a
//! network with no way out to one, from Home Assistant over MQTT
//! (Bridge::ask_time). On a unit whose ntpd keeps the time, the clock is never
//! off by enough to touch.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tokio::net::UdpSocket;

use crate::logf;

/// Set the clock when it is off by more.
pub const SKEW: Duration = Duration::from_secs(2);
/// Until the first answer.
pub const RETRY: Duration = Duration::from_secs(60);
pub const RECHECK: Duration = Duration::from_secs(6 * 3600);
/// Home Assistant's answer is late after this.
pub const ANSWER: Duration = Duration::from_secs(5);
/// 1900-01-01 to 1970-01-01, in seconds.
const NTP_EPOCH: u64 = 2_208_988_800;

/// host or host:port, as net.JoinHostPort would complete it.
fn with_port(server: &str) -> String {
    let has_port = match server.strip_prefix('[') {
        Some(rest) => rest.contains("]:"),
        None => server.matches(':').count() == 1,
    };
    match (has_port, server.contains(':')) {
        (true, _) => server.into(),
        (false, true) => format!("[{server}]:123"),
        (false, false) => format!("{server}:123"),
    }
}

/// Asks server (host or host:port) for the time.
pub async fn sntp(server: &str) -> std::io::Result<SystemTime> {
    let server = with_port(server);
    let addr = tokio::net::lookup_host(&server).await?.next()
        .ok_or_else(|| std::io::Error::other(format!("{server}: no address")))?;
    let socket = UdpSocket::bind(if addr.is_ipv4() { "0.0.0.0:0" } else { "[::]:0" }).await?;
    socket.connect(addr).await?;
    let mut request = [0u8; 48];
    request[0] = 0x23; // version 4, client
    let sent = SystemTime::now();
    socket.send(&request).await?;
    let mut response = [0u8; 48];
    let n = tokio::time::timeout(Duration::from_secs(5), socket.recv(&mut response)).await
        .map_err(|_| std::io::Error::other(format!("{server}: no answer")))??;
    let rtt = SystemTime::now().duration_since(sent).unwrap_or_default();
    if n < 48 || response[0] & 7 != 4 || response[1] == 0 {
        return Err(std::io::Error::other(format!("{server}: not a server answer")));
    }
    let seconds = u64::from(u32::from_be_bytes(response[40..44].try_into().unwrap()));
    let fraction = u64::from(u32::from_be_bytes(response[44..48].try_into().unwrap()));
    // RFC 4330's eras: with the top bit set, 1968-2036; clear, 2036-2104.
    // Zero is no time at all.
    let seconds = match seconds {
        0 => None,
        s if s & 0x8000_0000 != 0 => s.checked_sub(NTP_EPOCH),
        s => Some(s + (1 << 32) - NTP_EPOCH),
    }.ok_or_else(|| std::io::Error::other(format!("{server}: not a server answer")))?;
    let nanos = ((fraction * 1_000_000_000) >> 32) as u32;
    Ok(UNIX_EPOCH + Duration::new(seconds, nanos) + rtt / 2)
}

/// Sets the clock from server now and then, for good.
pub async fn keep_clock(server: String) {
    let mut failed = false;
    loop {
        match sntp(&server).await {
            Ok(t) => {
                failed = false;
                set_clock(t, &server);
                tokio::time::sleep(RECHECK).await;
            }
            Err(e) => {
                if !failed {
                    logf!("clock: {e} (retrying every {})", crate::logger::duration(RETRY));
                }
                failed = true;
                tokio::time::sleep(RETRY).await;
            }
        }
    }
}

/// How far t is ahead of now, negative when behind, in seconds.
fn offset(t: SystemTime) -> f64 {
    match t.duration_since(SystemTime::now()) {
        Ok(ahead) => ahead.as_secs_f64(),
        Err(behind) => -behind.duration().as_secs_f64(),
    }
}

/// Sets the clock to t if it is off.
pub fn set_clock(t: SystemTime, from: &str) {
    let off = offset(t);
    if off.abs() <= SKEW.as_secs_f64() {
        return;
    }
    let since = t.duration_since(UNIX_EPOCH).unwrap_or_default();
    let when = chrono::DateTime::from_timestamp(since.as_secs() as i64, since.subsec_nanos())
        .map_or_else(String::new, |d| d.format("%Y-%m-%d %H:%M:%S UTC").to_string());
    let tv = libc::timeval { tv_sec: since.as_secs() as libc::time_t, tv_usec: since.subsec_micros() as libc::suseconds_t };
    // SAFETY: settimeofday reads tv only; no timezone is passed.
    if unsafe { libc::settimeofday(&tv, std::ptr::null()) } != 0 {
        return logf!("clock: setting it to {when} from {from}: {}", std::io::Error::last_os_error());
    }
    let sign = if off < 0.0 { "-" } else { "" };
    logf!("clock: set to {when} from {from} (was off by {sign}{})", crate::logger::duration(Duration::from_secs(off.abs().round() as u64)));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn sntp_answer() {
        let server = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let addr = server.local_addr().unwrap();
        let want = UNIX_EPOCH + Duration::new(1_790_978_400, 500_000_000); // 2026-10-02 22:00:00.5 UTC
        tokio::spawn(async move {
            let mut request = [0u8; 48];
            let (_, from) = server.recv_from(&mut request).await.unwrap();
            assert_eq!(request[0], 0x23);
            let mut response = [0u8; 48];
            response[0] = 0x24; // version 4, server
            response[1] = 2; // stratum 2
            response[40..44].copy_from_slice(&((1_790_978_400 + NTP_EPOCH) as u32).to_be_bytes());
            response[44..48].copy_from_slice(&(1u32 << 31).to_be_bytes()); // half a second
            server.send_to(&response, from).await.unwrap();
        });
        let got = sntp(&addr.to_string()).await.unwrap();
        let d = got.duration_since(want).expect("plus half the round trip, not less");
        assert!(d < Duration::from_millis(100), "got {d:?} after the server's time");
    }

    /// Asks a loopback server that answers with response.
    async fn ask(response: Vec<u8>) -> std::io::Result<SystemTime> {
        let server = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let addr = server.local_addr().unwrap();
        tokio::spawn(async move {
            let mut request = [0u8; 48];
            let (_, from) = server.recv_from(&mut request).await.unwrap();
            server.send_to(&response, from).await.unwrap();
        });
        sntp(&addr.to_string()).await
    }

    #[tokio::test]
    async fn sntp_refuses_what_is_no_server_answer() {
        let answer = |mode: u8, stratum: u8, seconds: u32| {
            let mut r = vec![0u8; 48];
            r[0] = 0x20 | mode;
            r[1] = stratum;
            r[40..44].copy_from_slice(&seconds.to_be_bytes());
            r
        };
        let now = (1_790_978_400 + NTP_EPOCH) as u32;
        assert!(ask(answer(4, 2, now)).await.is_ok());
        for (why, response) in [
            ("a client's packet", answer(3, 2, now)),
            ("a kiss-o'-death (stratum 0)", answer(4, 0, now)),
            ("a time before 1970", answer(4, 2, 0x8000_0000)), // 1968
            ("no time at all", answer(4, 2, 0)),
            ("a short packet", answer(4, 2, now)[..47].to_vec()),
        ] {
            let e = ask(response).await.expect_err(why);
            assert!(e.to_string().ends_with("not a server answer"), "{why}: {e}");
        }
    }

    #[tokio::test]
    async fn sntp_after_the_2036_era_rollover() {
        let unix = 2_114_380_800u64; // 2037-01-01 00:00:00 UTC
        let mut r = vec![0u8; 48];
        r[0] = 0x24;
        r[1] = 2;
        r[40..44].copy_from_slice(&((unix + NTP_EPOCH) as u32).to_be_bytes()); // NTP era 1
        let got = ask(r).await.unwrap().duration_since(UNIX_EPOCH).unwrap();
        assert!(got >= Duration::from_secs(unix) && got < Duration::from_secs(unix + 1), "{got:?}");
    }

    #[test]
    fn offsets_ahead_and_behind() {
        let ahead = offset(SystemTime::now() + Duration::from_secs(100));
        let behind = offset(SystemTime::now() - Duration::from_secs(100));
        assert!((99.0..=100.0).contains(&ahead) && (-101.0..=-99.0).contains(&behind), "{ahead} {behind}");
    }

    #[test]
    fn servers_get_ntps_port() {
        assert_eq!(with_port("pool.ntp.org"), "pool.ntp.org:123");
        assert_eq!(with_port("10.0.0.2:1123"), "10.0.0.2:1123");
        assert_eq!(with_port("fd00::2"), "[fd00::2]:123");
        assert_eq!(with_port("[fd00::2]:124"), "[fd00::2]:124");
    }
}
