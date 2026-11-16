//! Logging as Go's log package did it for nestlocal: to stderr with the
//! date and microseconds, or with -syslog to the system log (daemon.info,
//! tag nestlocal), which on the unit is where everything else logs too. The
//! unit's syslogd keeps only its current file, so with -log-file the lines
//! also go to a file of nestlocal's own that survives a reboot.

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

/// The file the log is kept in beside the sink: at its limit it moves to
/// NAME.1, replacing the one before, and a new one starts.
struct File {
    path: std::path::PathBuf,
    file: std::fs::File,
    size: u64,
    limit: u64,
}

/// Where a file's size moves it to NAME.1: 256 KiB, so the two take 512 KiB.
const FILE_LIMIT: u64 = 256 * 1024;

static FILE: Mutex<Option<File>> = Mutex::new(None);

fn open(path: &std::path::Path) -> std::io::Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    options.open(path)
}

/// Keeps the log in `path` too from now on.
pub fn to_file(path: &str) -> std::io::Result<()> {
    *FILE.lock().unwrap() = Some(File::open(std::path::Path::new(path), FILE_LIMIT)?);
    Ok(())
}

impl File {
    fn open(path: &std::path::Path, limit: u64) -> std::io::Result<File> {
        let file = open(path)?;
        let size = file.metadata()?.len();
        Ok(File { path: path.into(), file, size, limit })
    }
}

/// Appends `text` to the file, moving it to NAME.1 first when `text` would
/// take it past its limit. A failure drops the file: the sink still logs.
fn keep(kept: &mut Option<File>, text: &str) {
    let Some(file) = kept.as_mut() else { return };
    let len = text.len() as u64;
    if file.size > 0 && file.size + len > file.limit {
        let mut old = file.path.clone().into_os_string();
        old.push(".1");
        match std::fs::rename(&file.path, &old).and_then(|()| open(&file.path)) {
            Ok(new) => (file.file, file.size) = (new, 0),
            Err(_) => return *kept = None,
        }
    }
    match file.file.write_all(text.as_bytes()) {
        Ok(()) => file.size += len,
        Err(_) => *kept = None,
    }
}

pub fn line(message: &str) {
    let now = chrono::Local::now();
    keep(&mut FILE.lock().unwrap(), &format!("{} {message}\n", now.format("%Y-%m-%d %H:%M:%S%.6f")));
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
    fn the_file_moves_to_name_1_at_its_limit() {
        let dir = std::env::temp_dir().join(format!("nestlocal-log-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("nestlocal.log");
        std::fs::write(&path, "before a restart\n").unwrap();
        let mut kept = Some(super::File::open(&path, 40).unwrap());
        super::keep(&mut kept, "0123456789\n");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "before a restart\n0123456789\n", "appended to");
        super::keep(&mut kept, "abcdefghijklmnopq\n");
        assert_eq!(std::fs::read_to_string(dir.join("nestlocal.log.1")).unwrap(), "before a restart\n0123456789\n");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "abcdefghijklmnopq\n");
        super::keep(&mut kept, "rstuvwxyz\n");
        super::keep(&mut kept, "0123456789abcdef\n");
        assert_eq!(
            std::fs::read_to_string(dir.join("nestlocal.log.1")).unwrap(),
            "abcdefghijklmnopq\nrstuvwxyz\n",
            "the older one replaced"
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "0123456789abcdef\n");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn durations_as_go_printed_them() {
        assert_eq!(super::duration(Duration::from_secs(300)), "5m0s");
        assert_eq!(super::duration(Duration::from_secs(45)), "45s");
        assert_eq!(super::duration(Duration::from_secs(6 * 3600)), "6h0m0s");
    }
}
