//! A device's patch bundle (unfr0zen_core::bundle), made from what the app
//! carries: a gen 3 run sends it to the unit, a gen 1 or gen 2 run patches
//! the slot from it on the host, and `unfr0zen-bundle pack` writes it for a
//! release.

use std::collections::BTreeMap;

use unfr0zen_core::bundle::{self, Bundle, Files};
use unfr0zen_core::ed25519_dalek::SigningKey;
use unfr0zen_core::{Device, Result};

use crate::assets;
use crate::flow::failure;

const PRE_PATCH: &[u8] = include_bytes!("../../../platform/diamond3/bundle/pre-patch.sh");
const POST_PATCH: &[u8] = include_bytes!("../../../platform/diamond3/bundle/post-patch.sh");

/// The bundle for a device: nestlocal, the libraries and objects the patch
/// installs or links, and Thread's and Doom's overlays. The gen 3's also has
/// unfr0zen-apply and the hook scripts, which run on the unit; the gen 1's
/// and gen 2's, patched on the host, the Sensors view helper and the SSH
/// server they lack.
pub fn contents(device: Device) -> Result<(Files, BTreeMap<String, u32>)> {
    match device {
        Device::Diamond3 => Ok(diamond3()),
        Device::J49 | Device::Diamond => Ok(omap()),
        Device::Onyx => Err(failure(format!("there is no patch bundle for the {}", device.name()))),
    }
}

/// What every bundle holds.
fn common() -> Files {
    Files::from([
        (bundle::NESTLOCAL.to_string(), (0o755, assets::nestlocal())),
        (bundle::WAKE_FILTER.to_string(), (0o755, assets::WAKE_FILTER.to_vec())),
        (bundle::LOCAL_CLOUD.to_string(), (0o755, assets::LOCAL_CLOUD.to_vec())),
        (bundle::SCREEN.to_string(), (0o644, assets::CUSTOM_SCREEN.to_vec())),
    ])
}

fn omap() -> (Files, BTreeMap<String, u32>) {
    let mut files = common();
    files.insert(bundle::RCS_SCREEN.into(), (0o644, assets::J49_RCS_SCREEN.to_vec()));
    let mut directories = BTreeMap::new();
    let thread = assets::with_thread_radio(&assets::thread(), crate::j49::assets::THREAD_RADIO,
                                           crate::j49::assets::THREAD_RADIO_IDENTITY);
    bundle::add_overlay(&mut files, &mut directories, bundle::THREAD, &thread);
    bundle::add_overlay(&mut files, &mut directories, bundle::DOOM, &assets::doom());
    bundle::add_overlay(&mut files, &mut directories, bundle::SSH, &crate::j49::assets::ssh_server());
    (files, directories)
}

fn diamond3() -> (Files, BTreeMap<String, u32>) {
    let mut files = common();
    files.extend([
        (bundle::APPLY.to_string(), (0o755, assets::apply())),
        (bundle::PRE_PATCH.to_string(), (0o755, PRE_PATCH.to_vec())),
        (bundle::POST_PATCH.to_string(), (0o755, POST_PATCH.to_vec())),
    ]);
    let mut directories = BTreeMap::new();
    let thread = assets::with_thread_radio(&assets::thread(), crate::diamond3::assets::THREAD_RADIO,
                                           crate::diamond3::assets::THREAD_RADIO_IDENTITY);
    bundle::add_overlay(&mut files, &mut directories, bundle::THREAD, &thread);
    bundle::add_overlay(&mut files, &mut directories, bundle::DOOM, &assets::doom());
    (files, directories)
}

/// The bundle as unpacking would give it, for a run that patches on the host.
pub fn assemble(device: Device, version: &str) -> Result<Bundle> {
    let (files, directories) = contents(device)?;
    Ok(Bundle::from_files(device, version, files, &directories))
}

/// The packed bundle, signed with key when given.
pub fn pack(device: Device, version: &str, key: Option<&SigningKey>) -> Result<Vec<u8>> {
    let (files, directories) = contents(device)?;
    Ok(bundle::pack(device, version, &files, &directories, key))
}

#[cfg(test)]
mod tests {
    use super::*;
    use unfr0zen_core::bundle::Settings;

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
        assert_eq!(options.local_cloud, assets::LOCAL_CLOUD);
        assert!(options.thread.unwrap().files.iter().any(|(name, _, _)| name == "usr/share/nestlocal/thread-rcp.bin"));
        assert_eq!(options.doom.unwrap().directories, [("usr/share/doom".to_string(), 0o755)]);
        assert!(pack(Device::Onyx, "1.0.0", None).is_err());
    }

    #[test]
    fn the_gen_2_bundle_brings_its_thread_image_ssh_server_and_sensors_view_and_nothing_to_run() {
        let bundle = assemble(Device::J49, "1.0.0").unwrap();
        assert!(bundle.file(bundle::APPLY).is_err() && bundle.file(bundle::PRE_PATCH).is_err());
        let settings = Settings { thread: true, ssh: true, external_temperature: true, matter: Some(bundle::MatterSettings {
            passcode: 20202021, discriminator: 3840 }), ..Settings::default() };
        let options = settings.options(&bundle).unwrap();
        assert_eq!(options.external_temperature_screen, assets::J49_RCS_SCREEN);
        assert!(options.ssh_server.unwrap().files.iter().any(|(name, _, _)| name == "usr/sbin/dropbearmulti"));
        let thread = options.thread.unwrap();
        let (_, _, image) = thread.files.iter().find(|(name, _, _)| name == "usr/share/nestlocal/thread-rcp.bin").unwrap();
        let em357 = assets::with_thread_radio(&assets::thread(), crate::j49::assets::THREAD_RADIO,
                                              crate::j49::assets::THREAD_RADIO_IDENTITY);
        assert!(em357.files.iter().any(|(name, _, data)| name == "usr/share/nestlocal/thread-rcp.bin" && data == image));
    }
}
