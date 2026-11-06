//! A moment written as the person's system writes dates and times: its
//! regional short date and time, where the platform offers them.

use std::time::SystemTime;

/// `time` in the system's short date and time format.
pub fn short(time: SystemTime) -> String {
    platform(time).unwrap_or_else(|| {
        chrono::DateTime::<chrono::Local>::from(time).format("%-d %b %Y %H:%M").to_string()
    })
}

/// A log folder's stamp (local date and time) in the same format.
pub fn stamp(stamp: &str) -> String {
    use chrono::TimeZone;
    chrono::NaiveDateTime::parse_from_str(stamp, "%Y%m%d-%H%M%S").ok()
        .and_then(|naive| chrono::Local.from_local_datetime(&naive).single())
        .map_or_else(|| stamp.to_string(), |local| short(local.into()))
}

#[cfg(target_os = "macos")]
fn platform(time: SystemTime) -> Option<String> {
    use objc2_foundation::{NSDate, NSDateFormatter, NSDateFormatterStyle};
    let secs = time.duration_since(SystemTime::UNIX_EPOCH).ok()?.as_secs_f64();
    let date = NSDate::dateWithTimeIntervalSince1970(secs);
    let text = NSDateFormatter::localizedStringFromDate_dateStyle_timeStyle(
        &date, NSDateFormatterStyle::ShortStyle, NSDateFormatterStyle::ShortStyle);
    Some(text.to_string())
}

#[cfg(windows)]
fn platform(time: SystemTime) -> Option<String> {
    use chrono::{Datelike, Timelike};
    use windows_sys::Win32::Foundation::SYSTEMTIME;
    use windows_sys::Win32::Globalization::{GetDateFormatEx, GetTimeFormatEx, DATE_SHORTDATE, TIME_NOSECONDS};
    let local = chrono::DateTime::<chrono::Local>::from(time);
    let system_time = SYSTEMTIME {
        wYear: local.year() as u16, wMonth: local.month() as u16,
        wDayOfWeek: local.weekday().num_days_from_sunday() as u16, wDay: local.day() as u16,
        wHour: local.hour() as u16, wMinute: local.minute() as u16, wSecond: local.second() as u16, wMilliseconds: 0,
    };
    let mut buffer = [0u16; 64];
    // A null locale name is the user's default.
    let text = |n: i32, buffer: &[u16]| (n > 1).then(|| String::from_utf16_lossy(&buffer[..n as usize - 1]));
    let date = text(unsafe { GetDateFormatEx(std::ptr::null(), DATE_SHORTDATE, &system_time, std::ptr::null(),
                                             buffer.as_mut_ptr(), buffer.len() as i32, std::ptr::null()) }, &buffer)?;
    let clock = text(unsafe { GetTimeFormatEx(std::ptr::null(), TIME_NOSECONDS, &system_time, std::ptr::null(),
                                              buffer.as_mut_ptr(), buffer.len() as i32) }, &buffer)?;
    Some(format!("{date} {clock}"))
}

#[cfg(all(unix, not(target_os = "macos")))]
fn platform(time: SystemTime) -> Option<String> {
    // strftime under the environment's locale, set once for times.
    static LOCALE: std::sync::Once = std::sync::Once::new();
    LOCALE.call_once(|| unsafe { libc::setlocale(libc::LC_TIME, c"".as_ptr()); });
    let secs = time.duration_since(SystemTime::UNIX_EPOCH).ok()?.as_secs() as libc::time_t;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    if unsafe { libc::localtime_r(&secs, &mut tm) }.is_null() {
        return None;
    }
    let mut buffer = [0 as libc::c_char; 64];
    let n = unsafe { libc::strftime(buffer.as_mut_ptr(), buffer.len(), c"%x %R".as_ptr(), &tm) };
    (n > 0).then(|| unsafe { std::ffi::CStr::from_ptr(buffer.as_ptr()) }.to_string_lossy().into_owned())
}

#[cfg(not(any(target_os = "macos", windows, unix)))]
fn platform(_: SystemTime) -> Option<String> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_moment_is_written_in_the_systems_format() {
        let text = short(SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_793_800_000));
        assert!(text.contains("26") || text.contains("2026"), "{text}");
        assert!(text.chars().any(|c| c == ':'), "{text}");
        let text = stamp("20261104-120000");
        assert!(text.contains("12"), "{text}");
        assert_eq!(stamp("not-a-stamp"), "not-a-stamp");
    }
}
