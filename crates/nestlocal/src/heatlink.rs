//! A Works With Nest boiler. Firmware 6.4-8 on a gen3 Display-3.x or
//! Display-5.x expects a Heat Link, which only connects through the radio
//! nestlocal holds with Zigbee or Thread. nlclient skips that check for a
//! Works With Nest boiler, which it reads at start from using_wwn_boiler in
//! its settings file: unfr0zen's rcS sets it before nlclient starts on a
//! Heat Link model when the run asked for it (--wwn-boiler). With it the unit
//! heats through W1 and keeps its safety temperature, but reports can_heat
//! false (bridge.rs reports it true).

use std::path::Path;

use crate::nlsettings;

pub const KEY: &str = "using_wwn_boiler";

/// Whether the settings file at path has using_wwn_boiler on, as nlclient
/// reads it: a number other than 0.
pub fn using(path: &Path) -> bool {
    std::fs::read(path).is_ok_and(|b| on(&b))
}

fn on(b: &[u8]) -> bool {
    nlsettings::value(b, KEY).and_then(|v| std::str::from_utf8(v).ok()?.trim().parse::<i64>().ok()).is_some_and(|v| v != 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_is_off() {
        assert!(!on(br#"<s key="using_wwn_boiler" value="0"/>"#) && on(br#"<s key="using_wwn_boiler" value="1"/>"#));
        assert!(!on(b"<settings/>") && !using(Path::new("/nonexistent/settings.config")));
    }
}
