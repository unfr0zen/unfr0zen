//! The j49 kernels' boot logo recoloured for the dark theme: the kernel with
//! the USB serial port from build/, and the stock kernel of a NAND sample.
use std::path::PathBuf;
use unfr0zen_core::j49::{flasher::Image, kernel, logo::{self, Logo}};
use unfr0zen_core::Slot;

fn kernels() -> Vec<(String, Vec<u8>)> {
    let mut kernels = Vec::new();
    let built = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../build/j49/uImage");
    if let Ok(image) = std::fs::read(&built) {
        kernels.push(("build/j49/uImage".into(), image));
    }
    if let Some(path) = std::env::var_os("J49_NAND") {
        let mut nand = Image::new(std::fs::read(&path).unwrap(), "sample").unwrap();
        let stock = kernel::read(&mut nand, Slot::Root0, false).unwrap().expect("a stock kernel in boot0");
        kernels.push(("the sample's stock kernel".into(), stock));
    }
    if kernels.is_empty() {
        assert!(std::env::var_os("NLCLIENT_REQUIRE_GOLDENS").is_none(), "strict checks need a j49 kernel");
        eprintln!("No j49 kernel; skipping the boot logo check");
    }
    kernels
}

#[test]
fn the_dark_logo_keeps_the_kernel_and_its_layout() {
    for (name, stock) in kernels() {
        assert_eq!(logo::logo(&stock).unwrap(), Some(Logo::Stock), "{name}");
        let dark = logo::darken(&stock).unwrap().unwrap_or_else(|| panic!("{name}: does not fit"));
        assert_eq!(dark.len(), stock.len(), "{name}");
        assert_eq!(logo::logo(&dark).unwrap(), Some(Logo::Dark), "{name}");
        assert_eq!(logo::darken(&dark).unwrap(), None, "{name}: already dark");
        // Only the header's CRCs and the compressed stream differ: the
        // decompressor before it and what the zImage links after it stay.
        let differ: Vec<usize> = (0..stock.len()).filter(|&i| stock[i] != dark[i]).collect();
        let (first, last) = (differ[0], *differ.last().unwrap());
        assert!(first == 4 || first == 24, "{name}: {first:#x}");
        let tail = stock.len() - last;
        assert!(tail > 8 && tail < 64, "{name}: {tail} bytes after the last change");
    }
}
