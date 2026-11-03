//! The computer a run is on, for the top of its run log: what a report needs
//! to reproduce a host-side problem. Only sources any user can read, and
//! nothing that names the person or the computer, since logs get posted.

/// One line per fact found, such as "OS: macOS 26.1 (25B78)".
pub(crate) fn describe() -> Vec<String> {
    facts().into_iter()
        .filter_map(|(label, value)| value.map(|v| v.trim().to_string()).filter(|v| !v.is_empty())
            .map(|value| format!("{label}: {value}")))
        .collect()
}

#[cfg(target_os = "macos")]
fn facts() -> Vec<(&'static str, Option<String>)> {
    fn run(program: &str, args: &[&str]) -> Option<String> {
        std::process::Command::new(program).args(args).output().ok().filter(|out| out.status.success())
            .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_string())
    }
    let sysctl = |name| run("/usr/sbin/sysctl", &["-n", name]);
    let build = run("/usr/bin/sw_vers", &["-buildVersion"]).unwrap_or_default();
    vec![
        ("OS", run("/usr/bin/sw_vers", &["-productVersion"]).map(|version| format!("macOS {version} ({build})"))),
        ("CPU", sysctl("machdep.cpu.brand_string")),
        ("Model", sysctl("hw.model")),
        ("Country", run("/usr/bin/defaults", &["read", "-g", "AppleLocale"]).and_then(|locale| country(&locale))),
        ("Rosetta", sysctl("sysctl.proc_translated").filter(|v| v == "1").map(|_| "this x86_64 build is translated".into())),
    ]
}

/// The country of a POSIX locale name such as en_CA.UTF-8: CA.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn country(locale: &str) -> Option<String> {
    let country: String = locale.split_once('_')?.1.chars().take_while(char::is_ascii_alphabetic).collect();
    (country.len() == 2).then(|| country.to_ascii_uppercase())
}

#[cfg(target_os = "linux")]
fn facts() -> Vec<(&'static str, Option<String>)> {
    let read = |path: &str| std::fs::read_to_string(path).ok();
    let field = |text: String, key: &str| text.lines()
        .find_map(|line| line.split_once(':').filter(|(k, _)| k.trim() == key).map(|(_, v)| v.trim().to_string()));
    let os = read("/etc/os-release").and_then(|text| text.lines()
        .find_map(|line| line.strip_prefix("PRETTY_NAME=")).map(|v| v.trim_matches('"').to_string()));
    let kernel = read("/proc/sys/kernel/osrelease").unwrap_or_default();
    let model = read("/sys/class/dmi/id/sys_vendor").zip(read("/sys/class/dmi/id/product_name"))
        .map(|(vendor, product)| format!("{} {}", vendor.trim(), product.trim()))
        .or_else(|| read("/proc/device-tree/model").map(|model| model.trim_end_matches('\0').to_string()));
    vec![
        ("OS", os.map(|os| format!("{os}, kernel {}", kernel.trim()))),
        ("CPU", read("/proc/cpuinfo").and_then(|text| field(text, "model name"))),
        ("Model", model),
        ("Country", ["LC_ALL", "LANG"].iter().find_map(|name| std::env::var(name).ok().filter(|v| !v.is_empty()))
            .and_then(|locale| country(&locale))),
    ]
}

#[cfg(windows)]
fn facts() -> Vec<(&'static str, Option<String>)> {
    const VERSION: &str = r"SOFTWARE\Microsoft\Windows NT\CurrentVersion";
    const BIOS: &str = r"HARDWARE\DESCRIPTION\System\BIOS";
    let machine = |key, value| registry(HKEY_LOCAL_MACHINE, key, value);
    let os = machine(VERSION, "CurrentBuild").map(|build| {
        // ProductName says Windows 10 on Windows 11 too; the build tells them apart.
        let name = if build.parse::<u32>().is_ok_and(|b| b >= 22000) { "Windows 11" } else { "Windows 10" };
        let release = machine(VERSION, "DisplayVersion").unwrap_or_default();
        let update = machine(VERSION, "UBR").unwrap_or_default();
        format!("{name} {release}, build {build}.{update}")
    });
    vec![
        ("OS", os),
        // The system's processor, also when this build runs emulated on it.
        ("CPU", std::env::var("PROCESSOR_IDENTIFIER").ok()),
        ("Model", machine(BIOS, "SystemManufacturer").zip(machine(BIOS, "SystemProductName"))
            .map(|(maker, product)| format!("{maker} {product}"))),
        ("Country", registry(HKEY_CURRENT_USER, r"Control Panel\International\Geo", "Name")),
    ]
}

#[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
fn facts() -> Vec<(&'static str, Option<String>)> {
    Vec::new()
}

#[cfg(windows)]
use windows_sys::Win32::System::Registry::{HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE};

/// A string or DWORD value under `hive`: the machine's, which any user can
/// read, or the user's own.
#[cfg(windows)]
fn registry(hive: HKEY, key: &str, value: &str) -> Option<String> {
    use windows_sys::Win32::System::Registry::{RegGetValueW, REG_DWORD, RRF_RT_REG_DWORD, RRF_RT_REG_SZ};
    let wide = |s: &str| s.encode_utf16().chain([0]).collect::<Vec<u16>>();
    let (key, value) = (wide(key), wide(value));
    let mut data = [0u16; 256];
    let (mut size, mut kind) = (std::mem::size_of_val(&data) as u32, 0);
    let status = unsafe {
        RegGetValueW(hive, key.as_ptr(), value.as_ptr(), RRF_RT_REG_SZ | RRF_RT_REG_DWORD,
                     &mut kind, data.as_mut_ptr().cast(), &mut size)
    };
    if status != 0 {
        return None;
    }
    if kind == REG_DWORD {
        return Some((u32::from(data[0]) | u32::from(data[1]) << 16).to_string());
    }
    Some(String::from_utf16_lossy(&data[..size as usize / 2]).trim_end_matches('\0').to_string())
}

#[cfg(test)]
mod tests {
    #[test]
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    fn the_country_is_the_locale_name_s_territory() {
        assert_eq!(super::country("en_CA.UTF-8").as_deref(), Some("CA"));
        assert_eq!(super::country("fr_fr@euro").as_deref(), Some("FR"));
        assert_eq!(super::country("en_US").as_deref(), Some("US"));
        assert_eq!(super::country("C.UTF-8"), None);
        assert_eq!(super::country("POSIX"), None);
    }

    #[test]
    #[cfg(any(target_os = "macos", target_os = "linux", windows))]
    fn names_the_operating_system_without_naming_the_computer() {
        let lines = super::describe();
        assert!(lines.iter().any(|line| line.starts_with("OS: ")), "{lines:?}");
        let host = std::process::Command::new("hostname").output().ok()
            .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_string());
        if let Some(host) = host.filter(|h| h.len() > 3) {
            assert!(!lines.iter().any(|line| line.contains(&host)), "{lines:?}");
        }
    }
}
