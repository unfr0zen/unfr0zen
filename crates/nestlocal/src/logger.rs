//! Logging as Go's log package did it for nestlocal: to stderr with the
//! date and microseconds, or with -syslog to the system log (daemon.info,
//! tag nestlocal), which on the unit is where everything else logs too.

use std::io::Write;
use std::sync::{Mutex, OnceLock};

enum Sink {
    Stderr,
    #[cfg(unix)]
    Syslog(std::os::unix::net::UnixDatagram),
}

static SINK: OnceLock<Mutex<Sink>> = OnceLock::new();

fn sink() -> &'static Mutex<Sink> {
    SINK.get_or_init(|| Mutex::new(Sink::Stderr))
}

/// Sends the log to syslog from now on, as log/syslog's New did: the local
/// socket, priority daemon.info.
#[cfg(unix)]
pub fn to_syslog() -> std::io::Result<()> {
    let mut last = std::io::Error::other("no syslog socket");
    for path in ["/dev/log", "/var/run/syslog", "/var/run/log"] {
        let socket = std::os::unix::net::UnixDatagram::unbound()?;
        match socket.connect(path) {
            Ok(()) => {
                *sink().lock().unwrap() = Sink::Syslog(socket);
                return Ok(());
            }
            Err(e) => last = e,
        }
    }
    Err(last)
}

pub fn line(message: &str) {
    let now = chrono::Local::now();
    let mut sink = sink().lock().unwrap();
    match &mut *sink {
        Sink::Stderr => {
            let _ = writeln!(std::io::stderr(), "{} {message}", now.format("%Y/%m/%d %H:%M:%S%.6f"));
        }
        #[cfg(unix)]
        Sink::Syslog(socket) => {
            // <daemon.info> timestamp tag[pid]: message, as log/syslog wrote it.
            let record = format!("<30>{} nestlocal[{}]: {message}\n", now.format("%b %e %H:%M:%S"), std::process::id());
            let _ = socket.send(record.as_bytes());
        }
    }
}

/// A duration as Go printed it in log lines: 45s, 5m0s, 6h0m0s.
pub fn duration(d: std::time::Duration) -> String {
    let s = d.as_secs();
    match (s / 3600, s % 3600 / 60, s % 60) {
        (0, 0, s) => format!("{s}s"),
        (0, m, s) => format!("{m}m{s}s"),
        (h, m, s) => format!("{h}h{m}m{s}s"),
    }
}

/// log.Printf.
#[macro_export]
macro_rules! logf {
    ($($arg:tt)*) => { $crate::logger::line(&format!($($arg)*)) };
}

/// log.Fatalf: log, then exit 1.
#[macro_export]
macro_rules! fatal {
    ($($arg:tt)*) => {{ $crate::logger::line(&format!($($arg)*)); std::process::exit(1) }};
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    #[test]
    fn durations_as_go_printed_them() {
        assert_eq!(super::duration(Duration::from_secs(300)), "5m0s");
        assert_eq!(super::duration(Duration::from_secs(45)), "45s");
        assert_eq!(super::duration(Duration::from_secs(6 * 3600)), "6h0m0s");
    }
}
