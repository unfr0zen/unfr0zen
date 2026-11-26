//! The Thermostat E's recovery U-Boot, validated against its build record.

pub const RECOVERY: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/u-boot-recovery-onyx.bin"));

pub const OFFICIAL_LOADER: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/u-boot-sdp-onyx.hab"));

pub(crate) const INITRAMFS_BOARD: &[u8] =
    include_bytes!(concat!(env!("UNFR0ZEN_ROOT"), "/platform/onyx/initramfs-board.sh"));

pub const RECOVERY_LOGO: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/onyx-recovery-logo.bgrx.gz"));

#[cfg(test)]
mod tests {
    #[test]
    fn recovery_identity_matches_the_firmware_config_and_is_not_a_diamond3() {
        let config = include_str!(concat!(env!("UNFR0ZEN_ROOT"), "/platform/onyx/uboot/onyx.config"));
        let number = |name: &str| {
            let value = config.lines().find_map(|line| line.strip_prefix(name)).unwrap();
            u16::from_str_radix(value.trim_start_matches("0x"), 16).unwrap()
        };
        let id = (number("CONFIG_USB_GADGET_VENDOR_NUM="), number("CONFIG_USB_GADGET_PRODUCT_NUM="));
        assert_eq!(id, crate::onyx::usb::RECOVERY);
        assert_eq!(crate::device::identify(id), Some(crate::Device::Onyx));
        assert_ne!(id, crate::diamond3::usb::RECOVERY);
    }
}
