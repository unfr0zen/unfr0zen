//! The command line, as Go's flag package read it: nestlocal.init and the
//! options file unfr0zen writes use its form (-mqtt tcp://..., -syslog,
//! -hold 290s), so this takes exactly that: one or two dashes, the value
//! after = or as the next argument, bare boolean flags, Go durations.

use std::time::Duration;

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Text,
    Bool,
    Duration,
}

/// Name, kind, default and help, in the order -h lists them.
const FLAGS: &[(&str, Kind, &str, &str)] = &[
    ("cert", Kind::Text, "/etc/nestlocal/server.crt", "TLS certificate (signed by the CA added to ca-bundle.pem)"),
    ("discovery", Kind::Text, "homeassistant", "Home Assistant discovery prefix"),
    ("doom", Kind::Bool, "false", "offer Doom on the UNFR0ZEN screen and launch it from there (doom.rs)"),
    ("hold", Kind::Duration, "290s", "how long an idle subscribe is held"),
    ("key", Kind::Text, "/etc/nestlocal/server.key", "TLS key"),
    ("listen", Kind::Text, "127.0.0.1:443", "HTTPS address nlclient's cloudregisterurl points at"),
    ("matter", Kind::Bool, "false", "serve the thermostat over Matter too (matter/)"),
    ("matter-discriminator", Kind::Text, "3840", "Matter discriminator, 0-4095"),
    ("matter-passcode", Kind::Text, "20202021", "Matter setup passcode"),
    ("matter-store", Kind::Text, "/media/data/nestlocal-matter", "where the Matter commissioning is kept"),
    ("matter-thread", Kind::Bool, "false", "Matter Thread network commissioning through the onboard radio"),
    ("zigbee", Kind::Bool, "false", "join a Zigbee network as a thermostat, through the unit's radio (zigbee/)"),
    ("zigbee-store", Kind::Text, "/media/data/nestlocal-zigbee.json", "where the Zigbee network is kept"),
    ("mqtt", Kind::Text, "", "MQTT broker, empty for none"),
    ("mqtt-ca", Kind::Text, "", "PEM file of the CA to verify the broker against; implies -mqtt-tls"),
    ("mqtt-password-file", Kind::Text, "", "file holding the MQTT password (kept off the command line)"),
    ("mqtt-tls", Kind::Bool, "false", "connect to the broker over TLS (-mqtt's tcp:// becomes ssl://)"),
    ("mqtt-user", Kind::Text, "", "MQTT user name, empty for none"),
    ("ntp-server", Kind::Text, "pool.ntp.org", "NTP server to set the unit's clock from when it is off (clock.rs), empty for none"),
    ("origin", Kind::Text, "https://127.0.0.1", "base URL handed to the device in /entry"),
    ("seed-structure", Kind::Text, "", "set structure_id in this nlclient settings file if it has none, then exit (nestlocal.init)"),
    ("serial", Kind::Text, "", "the unit's serial (default: the hostname, which the unit sets to it)"),
    ("stale", Kind::Duration, "5m", "availability goes offline when nlclient has made no request for this long"),
    ("state", Kind::Text, "/media/data/nestlocal.json", "bucket store"),
    ("syslog", Kind::Bool, "false", "log to syslog instead of stderr"),
    ("topic", Kind::Text, "nestlocal", "MQTT topic prefix"),
    ("wake-iface", Kind::Text, "wlan0", "WiFi interface the broker's packets wake the sleeping unit on (wake.rs); empty for none, which also lets the unit sleep with -matter"),
    ("v", Kind::Bool, "false", "log every request and answer"),
];

pub struct Config {
    pub cert: String,
    pub discovery: String,
    pub doom: bool,
    pub hold: Duration,
    pub key: String,
    pub listen: String,
    pub matter: bool,
    pub matter_discriminator: u16,
    pub matter_passcode: u32,
    pub matter_store: String,
    pub matter_thread: bool,
    pub zigbee: bool,
    pub zigbee_store: String,
    pub mqtt: String,
    pub mqtt_ca: String,
    pub mqtt_password_file: String,
    pub mqtt_tls: bool,
    pub mqtt_user: String,
    pub ntp_server: String,
    pub origin: String,
    pub seed_structure: String,
    pub serial: String,
    pub stale: Duration,
    pub state: String,
    pub syslog: bool,
    pub topic: String,
    pub verbose: bool,
    pub wake_iface: String,
}

/// A Go duration: "290s", "5m", "1h2m3.5s", "300ms", or "0".
pub fn parse_duration(s: &str) -> Result<Duration, String> {
    let bad = || format!("time: invalid duration {s:?}");
    let rest = s.strip_prefix('+').unwrap_or(s);
    if rest == "0" {
        return Ok(Duration::ZERO);
    }
    if rest.is_empty() || rest.starts_with('-') {
        return Err(bad());
    }
    let mut total = 0f64;
    let mut rest = rest;
    while !rest.is_empty() {
        let digits = rest.find(|c: char| !c.is_ascii_digit() && c != '.').ok_or_else(bad)?;
        let value: f64 = rest[..digits].parse().map_err(|_| bad())?;
        rest = &rest[digits..];
        let unit_len = rest.find(|c: char| c.is_ascii_digit() || c == '.').unwrap_or(rest.len());
        let scale = match &rest[..unit_len] {
            "ns" => 1e-9,
            "us" | "µs" | "μs" => 1e-6,
            "ms" => 1e-3,
            "s" => 1.0,
            "m" => 60.0,
            "h" => 3600.0,
            _ => return Err(bad()),
        };
        total += value * scale;
        rest = &rest[unit_len..];
    }
    Ok(Duration::from_secs_f64(total))
}

fn parse_bool(s: &str) -> Option<bool> {
    match s {
        "1" | "t" | "T" | "true" | "TRUE" | "True" => Some(true),
        "0" | "f" | "F" | "false" | "FALSE" | "False" => Some(false),
        _ => None,
    }
}

/// Go's -h listing.
pub fn usage() -> String {
    let mut out = String::from("Usage of nestlocal:\n");
    for (name, kind, default, help) in FLAGS {
        let typ = match kind {
            Kind::Text => " string",
            Kind::Duration => " duration",
            Kind::Bool => "",
        };
        out += &format!("  -{name}{typ}\n    \t{help}");
        match kind {
            Kind::Text if !default.is_empty() => out += &format!(" (default {default:?})"),
            Kind::Duration => out += &format!(" (default {})", crate::logger::duration(parse_duration(default).unwrap())),
            _ => {}
        }
        out.push('\n');
    }
    out
}


/// Parses args (without the program name). Err(None) asks for help.
pub fn parse(args: &[String]) -> Result<Config, Option<String>> {
    let mut values: Vec<(&str, String)> = FLAGS.iter().map(|(n, _, d, _)| (*n, d.to_string())).collect();
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        i += 1;
        if arg == "--" {
            break;
        }
        let Some(flag) = arg.strip_prefix("--").or_else(|| arg.strip_prefix('-')).filter(|f| !f.is_empty() && !arg.is_empty() && arg != "-") else {
            break; // the first non-flag argument ends the flags
        };
        if flag.starts_with('-') || flag.starts_with('=') {
            return Err(Some(format!("bad flag syntax: {arg}")));
        }
        let (name, inline) = match flag.split_once('=') {
            Some((n, v)) => (n, Some(v.to_string())),
            None => (flag, None),
        };
        if name == "h" || name == "help" {
            return Err(None);
        }
        let Some(&(_, kind, _, _)) = FLAGS.iter().find(|(n, ..)| *n == name) else {
            return Err(Some(format!("flag provided but not defined: -{name}")));
        };
        let value = match (kind, inline) {
            (Kind::Bool, None) => "true".into(),
            (Kind::Bool, Some(v)) => {
                parse_bool(&v).ok_or_else(|| Some(format!("invalid boolean value {v:?} for -{name}: parse error")))?;
                v
            }
            (_, Some(v)) => v,
            (_, None) => {
                let v = args.get(i).ok_or_else(|| Some(format!("flag needs an argument: -{name}")))?;
                i += 1;
                v.clone()
            }
        };
        if kind == Kind::Duration {
            parse_duration(&value).map_err(|e| Some(format!("invalid value {value:?} for flag -{name}: {e}")))?;
        }
        values.iter_mut().find(|(n, _)| *n == name).unwrap().1 = value;
    }

    let get = |name: &str| values.iter().find(|(n, _)| *n == name).unwrap().1.clone();
    let flag = |name: &str| parse_bool(&get(name)).unwrap_or(false);
    let duration = |name: &str| parse_duration(&get(name)).unwrap();
    if flag("matter-thread") && (flag("zigbee") || get("wake-iface").is_empty()) {
        return Err(Some("-matter-thread requires exclusive radio access (-zigbee=false) and a nonempty -wake-iface".into()));
    }
    let matter_passcode = get("matter-passcode").parse::<u32>().ok()
        .filter(|p| crate::matter::check_passcode(*p).is_ok())
        .ok_or_else(|| Some(format!("invalid value {:?} for flag -matter-passcode", get("matter-passcode"))))?;
    let matter_discriminator = get("matter-discriminator").parse::<u16>().ok().filter(|d| *d < 4096)
        .ok_or_else(|| Some(format!("invalid value {:?} for flag -matter-discriminator", get("matter-discriminator"))))?;
    Ok(Config {
        cert: get("cert"),
        discovery: get("discovery"),
        doom: flag("doom"),
        hold: duration("hold"),
        key: get("key"),
        listen: get("listen"),
        matter: flag("matter") || flag("matter-thread"),
        matter_discriminator,
        matter_passcode,
        matter_store: get("matter-store"),
        matter_thread: flag("matter-thread"),
        zigbee: flag("zigbee"),
        zigbee_store: get("zigbee-store"),
        mqtt: get("mqtt"),
        mqtt_ca: get("mqtt-ca"),
        mqtt_password_file: get("mqtt-password-file"),
        mqtt_tls: flag("mqtt-tls"),
        mqtt_user: get("mqtt-user"),
        ntp_server: get("ntp-server"),
        origin: get("origin"),
        seed_structure: get("seed-structure"),
        serial: get("serial"),
        stale: duration("stale"),
        state: get("state"),
        syslog: flag("syslog"),
        topic: get("topic"),
        verbose: flag("v"),
        wake_iface: get("wake-iface"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(s: &str) -> Vec<String> {
        s.split_whitespace().map(String::from).collect()
    }

    #[test]
    fn the_options_unfr0zen_writes() {
        let c = parse(&args("-syslog -mqtt tcp://broker:8883 -mqtt-user u -mqtt-password-file \
            /etc/nestlocal/mqtt.password -mqtt-tls -mqtt-ca /etc/nestlocal/mqtt-ca.pem")).ok().unwrap();
        assert!(c.syslog && c.mqtt_tls && !c.verbose && !c.doom);
        assert_eq!((c.mqtt.as_str(), c.mqtt_user.as_str()), ("tcp://broker:8883", "u"));
        assert!(parse(&args("-mqtt= -doom")).ok().unwrap().doom);
        assert_eq!(c.mqtt_ca, "/etc/nestlocal/mqtt-ca.pem");
        assert_eq!((c.hold, c.stale, c.listen.as_str()), (Duration::from_secs(290), Duration::from_secs(300), "127.0.0.1:443"));
    }

    #[test]
    fn go_forms() {
        let c = parse(&args("--v --hold=1m30s -mqtt= -mqtt-tls=false -serial S")).ok().unwrap();
        assert!(c.verbose && !c.mqtt_tls);
        assert_eq!((c.hold, c.mqtt.as_str(), c.serial.as_str()), (Duration::from_secs(90), "", "S"));
        assert_eq!(parse(&args("-nope")).err().unwrap().unwrap(), "flag provided but not defined: -nope");
        assert_eq!(parse(&args("-mqtt")).err().unwrap().unwrap(), "flag needs an argument: -mqtt");
        assert!(parse(&args("-h")).err().unwrap().is_none());
        assert!(parse(&args("-hold 5x")).is_err());
        // The first argument that is not a flag ends them.
        assert_eq!(parse(&args("-v extra -serial S")).ok().unwrap().serial, "");
    }

    #[test]
    fn flag_syntax_errors() {
        assert_eq!(parse(&args("---v")).err().unwrap().unwrap(), "bad flag syntax: ---v");
        assert_eq!(parse(&args("-=v")).err().unwrap().unwrap(), "bad flag syntax: -=v");
        assert!(parse(&args("-v=maybe")).err().unwrap().unwrap().starts_with("invalid boolean value \"maybe\""));
        assert!(parse(&args("-stale -5m")).err().unwrap().unwrap().starts_with("invalid value \"-5m\" for flag -stale"));
        // A bare boolean takes no value: what follows is the next argument.
        let c = parse(&args("-syslog false -serial S")).ok().unwrap();
        assert!(c.syslog);
        assert_eq!(c.serial, "");
        // -- ends the flags; a value that looks like a flag is still a value.
        let c = parse(&args("-serial -v -- -topic t")).ok().unwrap();
        assert_eq!((c.serial.as_str(), c.verbose, c.topic.as_str()), ("-v", false, "nestlocal"));
    }

    #[test]
    fn matter_settings() {
        let c = parse(&args("-matter -matter-passcode 34970112 -matter-discriminator 1234")).ok().unwrap();
        assert!(c.matter);
        assert_eq!((c.matter_passcode, c.matter_discriminator), (34970112, 1234));
        let c = parse(&args("")).ok().unwrap();
        assert!(!c.matter);
        assert_eq!((c.matter_passcode, c.matter_discriminator), (20202021, 3840));
        assert!(parse(&args("-matter-passcode 12345678")).is_err());
        assert!(parse(&args("-matter-discriminator 4096")).is_err());
    }

    #[test]
    fn thread_requires_exclusive_radio_and_sleep_registration() {
        let c = parse(&args("-matter-thread")).ok().unwrap();
        assert!(c.matter && c.matter_thread && !c.zigbee);
        assert!(parse(&args("-matter-thread -zigbee")).is_err());
        assert!(parse(&args("-matter-thread -wake-iface=")).is_err());
        assert!(!parse(&args("-matter")).ok().unwrap().matter_thread);
    }

    #[test]
    fn durations() {
        assert_eq!(parse_duration("290s").unwrap(), Duration::from_secs(290));
        assert_eq!(parse_duration("1.5h").unwrap(), Duration::from_secs(5400));
        assert_eq!(parse_duration("300ms").unwrap(), Duration::from_millis(300));
        assert_eq!(parse_duration("0").unwrap(), Duration::ZERO);
        assert!(parse_duration("5").is_err() && parse_duration("").is_err());
        assert_eq!(parse_duration("1h2m3.5s").unwrap(), Duration::from_millis(3_723_500));
        assert_eq!(parse_duration("+90s").unwrap(), Duration::from_secs(90));
        for bad in ["-5s", "5x", "s", "1.2.3s", "5 s"] {
            assert!(parse_duration(bad).is_err(), "{bad:?}");
        }
        assert!(usage().contains("  -hold duration\n    \thow long an idle subscribe is held (default 4m50s)\n"));
        // No broker unless one is named.
        assert_eq!(parse(&args("")).ok().unwrap().mqtt, "");
        assert!(usage().contains("  -mqtt string\n    \tMQTT broker, empty for none\n"));
    }
}
