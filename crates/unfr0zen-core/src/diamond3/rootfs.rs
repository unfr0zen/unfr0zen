//! The gen 3's root filesystem setup.

use crate::common::rootfs;
use crate::setup::{Options, Rootfs, Tree};
use crate::Result;

pub use rootfs::set_up_ssh;
pub use rootfs::FILES;

pub(crate) static ROOTFS: Rootfs =
    Rootfs { files: &FILES, shadow_modes: &[0o640], optional_files: |_| Vec::new(), set_up };

fn set_up(tree: &mut Tree, options: &mut Options) -> Result<()> {
    rootfs::set_up_access(tree, options, true, &[])
}
