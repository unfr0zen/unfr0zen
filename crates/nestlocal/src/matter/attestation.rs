//! Each generation's Matter identity: the test vendor, a product of its own
//! (package::product) and a development attestation certificate (DAC) for
//! that product. Controllers check that the DAC's product is the one Basic
//! Information reports. The gen 3 keeps rs-matter's test DAC for 0x8001; the
//! gen 2's and gen 1's are the development DACs connectedhomeip publishes
//! (credentials/development/attestation), issued by the same PAI, the
//! vendor's development PAI with no product, and covered by the same
//! certification declaration (products 0x8000 to 0x8063).

use rs_matter::crypto::{CanonPkcPublicKeyRef, CanonPkcSecretKeyRef};
use rs_matter::dm::clusters::dev_att::DeviceAttestation;
use rs_matter::dm::devices::test::{TestDeviceAttestation, TEST_DEV_ATT};
use rs_matter::utils::sync::DynBase;

static TEST: TestDeviceAttestation = TEST_DEV_ATT;

pub struct Attestation {
    pub product: u16,
    /// The DAC and its key (SEC1 DER), or none for rs-matter's.
    own: Option<(&'static [u8], &'static [u8; 121])>,
}

const GEN2: Attestation = Attestation {
    product: 0x8002,
    own: Some((
        include_bytes!("attestation/Matter-Development-DAC-FFF1-8002-Cert.der"),
        include_bytes!("attestation/Matter-Development-DAC-FFF1-8002-Key.der"),
    )),
};
const GEN1: Attestation = Attestation {
    product: 0x8003,
    own: Some((
        include_bytes!("attestation/Matter-Development-DAC-FFF1-8003-Cert.der"),
        include_bytes!("attestation/Matter-Development-DAC-FFF1-8003-Key.der"),
    )),
};
const GEN3: Attestation = Attestation { product: 0x8001, own: None };

/// The attestation of the device named `device`, as its product says.
pub fn of(device: &str) -> &'static Attestation {
    static ALL: [Attestation; 3] = [GEN3, GEN2, GEN1];
    let product = crate::package::product(device);
    ALL.iter().find(|a| a.product == product).expect("every product has a DAC")
}

impl DynBase for Attestation {}

impl DeviceAttestation for Attestation {
    fn cert_declaration(&self) -> &[u8] {
        TEST.cert_declaration()
    }

    fn pai(&self) -> &[u8] {
        TEST.pai()
    }

    fn dac(&self) -> &[u8] {
        self.own.map_or_else(|| TEST.dac(), |(dac, _)| dac)
    }

    fn dac_pub_key(&self) -> CanonPkcPublicKeyRef<'_> {
        match self.own {
            // SEC1: the public key is its last 65 bytes.
            Some((_, key)) => CanonPkcPublicKeyRef::new(key[56..].try_into().unwrap()),
            None => TEST.dac_pub_key(),
        }
    }

    fn dac_priv_key(&self) -> CanonPkcSecretKeyRef<'_> {
        match self.own {
            // SEC1: version, then the 32-byte private key from byte 7.
            Some((_, key)) => CanonPkcSecretKeyRef::new(key[7..39].try_into().unwrap()),
            None => TEST.dac_priv_key(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_generation_attests_its_own_product() {
        for (device, product) in [("diamond3", 0x8001u16), ("j49", 0x8002), ("diamond", 0x8003)] {
            let attestation = of(device);
            assert_eq!(attestation.product, product);
            // The DAC names the product: Matter's product ID attribute, as text.
            let text = format!("{product:04X}");
            assert!(attestation.dac().windows(4).any(|w| w == text.as_bytes()), "{device}'s DAC is for {text}");
            // Its key pair is the DAC's: the certificate carries the public key.
            let public = attestation.dac_pub_key();
            assert!(attestation.dac().windows(65).any(|w| w == public.access()), "{device}'s key is its DAC's");
            assert_eq!(attestation.pai(), TEST.pai());
        }
        for (_, key) in [GEN2.own.unwrap(), GEN1.own.unwrap()] {
            // SEC1 ECPrivateKey: the private key and public key where read.
            assert_eq!((&key[5..7], &key[51..56]), (&[0x04, 0x20][..], &[0xA1, 0x44, 0x03, 0x42, 0x00][..]));
        }
    }
}
