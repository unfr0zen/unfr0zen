//! A device's patch bundle (unfr0zen_core::bundle), made from what the app
//! carries: the run sends it to the unit, and `unfr0zen-bundle pack` writes
//! the same bytes, signed, for a release.

use std::collections::BTreeMap;

use unfr0zen_core::bundle::{self, Files};
use unfr0zen_core::ed25519_dalek::SigningKey;
use unfr0zen_core::{Device, Result};

use crate::assets;
use crate::flow::failure;

const PRE_PATCH: &[u8] = include_bytes!("../../../platform/diamond3/bundle/pre-patch.sh");
const POST_PATCH: &[u8] = include_bytes!("../../../platform/diamond3/bundle/post-patch.sh");

/// The bundle for a device: unfr0zen-apply, nestlocal, the libraries and
/// objects the patch installs or links, the hook scripts, and Thread's and
/// Doom's overlays.
pub fn contents(device: Device) -> Result<(Files, BTreeMap<String, u32>)> {
    if device != Device::Diamond3 {
        return Err(failure(format!("there is no patch bundle for the {}", device.name())));
    }
    let mut files = Files::from([
        (bundle::APPLY.to_string(), (0o755, assets::apply())),
        (bundle::NESTLOCAL.to_string(), (0o755, assets::nestlocal())),
        (bundle::WAKE_FILTER.to_string(), (0o755, assets::WAKE_FILTER.to_vec())),
        (bundle::SCREEN.to_string(), (0o644, assets::CUSTOM_SCREEN.to_vec())),
        (bundle::PRE_PATCH.to_string(), (0o755, PRE_PATCH.to_vec())),
        (bundle::POST_PATCH.to_string(), (0o755, POST_PATCH.to_vec())),
    ]);
    let mut directories = BTreeMap::new();
    let thread = assets::with_thread_radio(&assets::thread(), crate::diamond3::assets::THREAD_RADIO,
                                           crate::diamond3::assets::THREAD_RADIO_IDENTITY);
    bundle::add_overlay(&mut files, &mut directories, bundle::THREAD, &thread);
    bundle::add_overlay(&mut files, &mut directories, bundle::DOOM, &assets::doom());
    Ok((files, directories))
}

/// The packed bundle, signed with key when given.
pub fn pack(device: Device, version: &str, key: Option<&SigningKey>) -> Result<Vec<u8>> {
    let (files, directories) = contents(device)?;
    Ok(bundle::pack(device, version, &files, &directories, key))
}

#[cfg(test)]
mod tests {
    use super::*;
    use unfr0zen_core::bundle::{Bundle, Settings};

    #[test]
    fn the_diamond3_bundle_holds_what_every_option_takes() {
        let data = pack(Device::Diamond3, "1.0.0", None).unwrap();
        let bundle = Bundle::unpack(&data).unwrap();
        assert_eq!(bundle.manifest.device, "diamond3");
        assert!(bundle.file(bundle::APPLY).unwrap().starts_with(b"\x7fELF"));
        let settings = Settings { thread: true, doom: true, matter: Some(bundle::MatterSettings { passcode: 20202021, discriminator: 3840 }),
                                  ..Settings::default() };
        let options = settings.options(&bundle).unwrap();
        assert_eq!(options.wake_filter, assets::WAKE_FILTER);
        assert!(options.thread.unwrap().files.iter().any(|(name, _, _)| name == "usr/share/nestlocal/thread-rcp.bin"));
        assert_eq!(options.doom.unwrap().directories, [("usr/share/doom".to_string(), 0o755)]);
        assert!(pack(Device::J49, "1.0.0", None).is_err());
    }
}
