//! A device's setup bundle (unfr0zen_core::bundle), made from what the app
//! carries: a gen 3 run sends it to the unit, a gen 1 or gen 2 run sets up
//! the slot from it on the host, and `unfr0zen-bundle pack` writes it for a
//! release.

use std::collections::BTreeMap;

use unfr0zen_core::bundle::{self, Bundle, Files};
use unfr0zen_core::ed25519_dalek::SigningKey;
use unfr0zen_core::{Device, Result};

use crate::assets;
use crate::flow::failure;

/// The bundle for a device: nestlocal, the libraries and objects the setup
/// installs or links, Thread's and Doom's overlays, and unfr0zen-apply and
/// the hook scripts, which run on the unit. The gen 3's also has the
/// customizable themes' overlay; the gen 1's and gen 2's, alike but each
/// named for its device so an update sets up the right one, the Sensors
/// view helper and the SSH server they lack.
pub fn contents(device: Device) -> Result<(Files, BTreeMap<String, u32>)> {
    match device {
        Device::Diamond3 => Ok(diamond3()),
        Device::J49 | Device::Diamond => Ok(omap()),
        Device::Onyx => Err(failure(format!("there is no setup bundle for the {}", device.name()))),
    }
}

/// What every bundle holds.
fn common() -> Files {
    Files::from([
        (bundle::NESTLOCAL.to_string(), (0o755, assets::nestlocal())),
        (bundle::WAKE_FILTER.to_string(), (0o755, assets::WAKE_FILTER.to_vec())),
        (bundle::LOCAL_CLOUD.to_string(), (0o755, assets::LOCAL_CLOUD.to_vec())),
        (bundle::SCREEN.to_string(), (0o644, assets::CUSTOM_SCREEN.to_vec())),
        (bundle::SCREEN_LEGACY.to_string(), (0o644, assets::CUSTOM_SCREEN_LEGACY.to_vec())),
    ])
}

fn omap() -> (Files, BTreeMap<String, u32>) {
    let mut files = common();
    files.extend([
        (bundle::APPLY.to_string(), (0o755, assets::apply())),
        (bundle::PRE_SETUP.to_string(), (0o755, crate::j49::assets::PRE_SETUP.to_vec())),
        (bundle::POST_SETUP.to_string(), (0o755, crate::diamond3::assets::POST_SETUP.to_vec())),
    ]);
    files.insert(bundle::RCS_SCREEN.into(), (0o644, assets::J49_RCS_SCREEN.to_vec()));
    let mut directories = BTreeMap::new();
    let thread = assets::with_thread_radio(
        &assets::thread(),
        crate::j49::assets::THREAD_RADIO,
        crate::j49::assets::THREAD_RADIO_IDENTITY,
    );
    bundle::add_overlay(&mut files, &mut directories, bundle::THREAD, &thread);
    bundle::add_overlay(&mut files, &mut directories, bundle::DOOM, &assets::doom());
    bundle::add_overlay(&mut files, &mut directories, bundle::SSH, &crate::j49::assets::ssh_server());
    bundle::add_overlay(&mut files, &mut directories, bundle::THEME, &assets::theme());
    (files, directories)
}

fn diamond3() -> (Files, BTreeMap<String, u32>) {
    let mut files = common();
    files.extend([
        (bundle::APPLY.to_string(), (0o755, assets::apply())),
        (bundle::PRE_SETUP.to_string(), (0o755, crate::diamond3::assets::PRE_SETUP.to_vec())),
        (bundle::POST_SETUP.to_string(), (0o755, crate::diamond3::assets::POST_SETUP.to_vec())),
    ]);
    let mut directories = BTreeMap::new();
    let thread = assets::with_thread_radio(
        &assets::thread(),
        crate::diamond3::assets::THREAD_RADIO,
        crate::diamond3::assets::THREAD_RADIO_IDENTITY,
    );
    bundle::add_overlay(&mut files, &mut directories, bundle::THREAD, &thread);
    bundle::add_overlay(&mut files, &mut directories, bundle::DOOM, &assets::doom());
    bundle::add_overlay(&mut files, &mut directories, bundle::THEME, &assets::theme());
    (files, directories)
}

/// The bundle as unpacking would give it, for a run that sets up on the host.
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
        assert_eq!(bundle.file(bundle::SCREEN).unwrap(), assets::CUSTOM_SCREEN);
        assert_eq!(bundle.file(bundle::SCREEN_LEGACY).unwrap(), assets::CUSTOM_SCREEN_LEGACY);
        assert!(bundle.file(bundle::APPLY).unwrap().starts_with(b"\x7fELF"));
        let settings = Settings {
            thread: true,
            doom: true,
            theme: true,
            matter: Some(bundle::MatterSettings { passcode: 20202021, discriminator: 3840 }),
            ..Settings::default()
        };
        let options = settings.options(&bundle).unwrap();
        assert_eq!(options.wake_filter, assets::WAKE_FILTER);
        assert_eq!(options.local_cloud, assets::LOCAL_CLOUD);
        assert!(options.thread.unwrap().files.iter().any(|(name, _, _)| name == "usr/share/nestlocal/thread-rcp.bin"));
        assert_eq!(options.doom.unwrap().directories, [("usr/share/doom".to_string(), 0o755)]);
        let theme = options.theme.unwrap();
        let (libraries, themes): (Vec<_>, Vec<_>) = theme.files.iter().partition(|(name, _, _)| name.ends_with(".so"));
        assert!(libraries.len() == 2 && libraries.iter().all(|(_, _, data)| data.starts_with(b"\x7fELF")));
        assert!(themes.iter().all(|(name, _, _)| name.starts_with("media/scratch/.unfr0zen/themes/")));
        assert!(themes.iter().any(|(name, _, _)| name.ends_with("/theme.lua")));
        assert!(pack(Device::Onyx, "1.0.0", None).is_err());
    }

    #[test]
    fn the_gen_1_bundle_is_the_gen_2s_named_for_the_gen_1() {
        let (gen1, gen2) = (
            Bundle::unpack(&pack(Device::Diamond, "1.0.0", None).unwrap()).unwrap(),
            Bundle::unpack(&pack(Device::J49, "1.0.0", None).unwrap()).unwrap(),
        );
        assert_eq!((gen1.manifest.device.as_str(), gen2.manifest.device.as_str()), ("diamond", "j49"));
        assert_eq!(gen1.files, gen2.files);
    }

    #[test]
    fn the_gen_2_bundle_brings_its_thread_image_ssh_server_sensors_view_themes_and_engine() {
        let bundle = assemble(Device::J49, "1.0.0").unwrap();
        assert!(bundle.file(bundle::APPLY).unwrap().starts_with(b"\x7fELF"));
        assert_eq!(bundle.file(bundle::PRE_SETUP).unwrap(), crate::j49::assets::PRE_SETUP);
        let settings = Settings {
            thread: true,
            ssh: true,
            external_temperature: true,
            theme: true,
            matter: Some(bundle::MatterSettings { passcode: 20202021, discriminator: 3840 }),
            ..Settings::default()
        };
        let options = settings.options(&bundle).unwrap();
        assert_eq!(options.external_temperature_screen, assets::J49_RCS_SCREEN);
        let theme = options.theme.unwrap();
        assert!(theme.files.iter().any(|(name, _, data)| name.ends_with(".so") && data.starts_with(b"\x7fELF")));
        assert!(theme.files.iter().any(|(name, _, _)| name.ends_with("/theme.lua")));
        assert!(options.ssh_server.unwrap().files.iter().any(|(name, _, _)| name == "usr/sbin/dropbearmulti"));
        let thread = options.thread.unwrap();
        let (_, _, image) =
            thread.files.iter().find(|(name, _, _)| name == "usr/share/nestlocal/thread-rcp.bin").unwrap();
        let em357 = assets::with_thread_radio(
            &assets::thread(),
            crate::j49::assets::THREAD_RADIO,
            crate::j49::assets::THREAD_RADIO_IDENTITY,
        );
        assert!(em357
            .files
            .iter()
            .any(|(name, _, data)| name == "usr/share/nestlocal/thread-rcp.bin" && data == image));
    }
}
