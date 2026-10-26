//! Checks against a real j49 NAND image (raw 2112-byte pages), which carries
//! a unit's credentials and so is never in the repository: J49_NAND names
//! it. Without it these tests pass without checking anything.

use std::path::PathBuf;

use unfr0zen_core::j49::jffs2;
use unfr0zen_core::j49::nand::{self, Environment, PAGE, PAGE_RAW};
use unfr0zen_core::shell::Volume;
use unfr0zen_core::Slot;

fn image() -> Option<Vec<u8>> {
    let path = PathBuf::from(std::env::var_os("J49_NAND")?);
    Some(std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display())))
}

fn pages(image: &[u8], offset: usize, length: usize) -> &[u8] {
    &image[offset / PAGE * PAGE_RAW..(offset + length) / PAGE * PAGE_RAW]
}

#[test]
fn the_environment_names_both_slots() {
    let Some(image) = image() else { return };
    let copies: Vec<Option<Environment>> = nand::ENV_COPIES.iter().map(|&offset| {
        let data: Vec<u8> = pages(&image, offset, nand::ENV_SIZE).chunks(PAGE_RAW)
            .flat_map(|raw| nand::correct_page(raw).unwrap().0).collect();
        Environment::parse(&data)
    }).collect();
    let (_, env) = nand::newest(&copies).unwrap();
    let order = env.boot_order().unwrap();
    assert_ne!(order[0], order[1]);
    // The bytes U-Boot would write back are the bytes it wrote.
    let index = copies.iter().position(|c| c.as_ref() == Some(env)).unwrap();
    let data: Vec<u8> = pages(&image, nand::ENV_COPIES[index], nand::ENV_SIZE).chunks(PAGE_RAW)
        .flat_map(|raw| nand::correct_page(raw).unwrap().0).collect();
    let written = env.to_bytes().unwrap();
    let end = 5 + env.values.iter().map(|(k, v)| k.len() + v.len() + 2).sum::<usize>() + 1;
    assert_eq!(written[..end], data[..end]);
}

#[test]
fn every_root_page_corrects_and_reencodes() {
    let Some(image) = image() else { return };
    for slot in [Slot::Root0, Slot::Root1] {
        let (offset, length) = nand::slot_partition(slot);
        let (mut fixed, mut written) = (0, 0);
        for raw in pages(&image, offset, length).chunks(PAGE_RAW) {
            let (data, bits) = nand::correct_page(raw).unwrap();
            fixed += bits;
            if !nand::erased(raw) {
                written += 1;
                assert_eq!(nand::correct_page(&nand::encode_page(&data, &raw[PAGE..])).unwrap(), (data, 0));
            }
        }
        assert!(written > 0, "{slot} has no programmed page");
        eprintln!("{slot}: {written} programmed pages, {fixed} bits corrected");
    }
}


fn slot(image: &[u8], slot: Slot) -> jffs2::Slot {
    let (offset, length) = nand::slot_partition(slot);
    jffs2::Slot::load(pages(image, offset, length).to_vec()).unwrap()
}

/// J49_TREE names a directory with the slots extracted by another reader
/// (root0/ and root1/), for a file-by-file comparison. Names that differ only
/// in case share a file on a case-insensitive host, so a name the slot lacks
/// is only counted; a reader that ignores truncation may give more than the
/// file holds, so the file must be a prefix of what it extracted.
#[test]
fn every_file_reads_as_another_reader_extracts_it() {
    let (Some(image), Some(tree)) = (image(), std::env::var_os("J49_TREE").map(PathBuf::from)) else { return };
    for (which, name) in [(Slot::Root0, "root0"), (Slot::Root1, "root1")] {
        let mut volume = slot(&image, which);
        let (mut files, mut missing, mut pending) = (0, 0, vec![tree.join(name)]);
        while let Some(dir) = pending.pop() {
            for entry in std::fs::read_dir(&dir).unwrap() {
                let path = entry.unwrap().path();
                let kind = std::fs::symlink_metadata(&path).unwrap().file_type();
                let relative = path.strip_prefix(tree.join(name)).unwrap().to_string_lossy().into_owned();
                if kind.is_dir() {
                    pending.push(path);
                } else if kind.is_file() {
                    match volume.read(&relative).unwrap() {
                        Some((_, _, data)) => {
                            assert!(std::fs::read(&path).unwrap().starts_with(&data), "{name}/{relative} differs");
                            files += 1;
                        }
                        None => {
                            if !relative.starts_with("usr/share/terminfo/") && !relative.starts_with("usr/share/zoneinfo/") {
                                eprintln!("{name}/{relative} not in the slot");
                            }
                            missing += 1;
                        }
                    }
                }
            }
        }
        eprintln!("{name}: {files} files match, {missing} not in the slot under that name");
    }
}

#[test]
fn a_file_appended_to_a_real_slot_reads_back() {
    let Some(image) = image() else { return };
    let mut volume = slot(&image, Slot::Root1);
    let inittab = volume.read("etc/inittab").unwrap().unwrap();
    let mut text = inittab.2.clone();
    text.extend_from_slice(b"# unfr0zen\n");
    volume.write("etc/inittab", &text, &inittab.0, inittab.1, &Default::default()).unwrap();
    volume.write("etc/nestlocal/options", b"-v\n", "0:0", 0o600, &Default::default()).unwrap();
    let plan = volume.programmed().unwrap();
    assert!(!plan.after.is_empty() && plan.erase.is_empty());
    let mut back = jffs2::Slot::load(plan.raw).unwrap();
    assert_eq!(back.read("etc/inittab").unwrap().unwrap(), (inittab.0, inittab.1, text));
    assert_eq!(back.read("etc/nestlocal/options").unwrap().unwrap().2, b"-v\n");
    assert_eq!(back.shell_binary().unwrap(), "0 755");
}

/// A nestlocal too big to go in beside the one a slot holds takes the old
/// one's blocks: its name is removed first, those blocks are erased, and the
/// rest of the slot reads as before.
#[test]
fn a_new_nestlocal_takes_the_old_ones_blocks_when_it_does_not_fit_beside_them() {
    let Some(image) = image() else { return };
    let mut volume = slot(&image, Slot::Root0);
    let Some(old) = volume.read("nestlabs/sbin/nestlocal").unwrap() else { return };
    let inittab = volume.read("etc/inittab").unwrap().unwrap();
    let mut x = 1u32;
    let new: Vec<u8> = (0..7 << 20).map(|_| { x ^= x << 13; x ^= x >> 17; x ^= x << 5; x as u8 }).collect();
    volume.write("nestlabs/sbin/nestlocal", &new, &old.0, old.1, &Default::default()).unwrap();
    let plan = volume.programmed().unwrap();
    eprintln!("root0: {} pages before, {} blocks erased, {} pages after", plan.before.len(), plan.erase.len(), plan.after.len());
    assert!(!plan.erase.is_empty() && !plan.before.is_empty());
    let mut back = jffs2::Slot::load(plan.raw.clone()).unwrap();
    assert_eq!(back.read("nestlabs/sbin/nestlocal").unwrap().unwrap(), (old.0, old.1, new));
    assert_eq!(back.read("etc/inittab").unwrap().unwrap(), inittab);
    assert_eq!(back.shell_binary().unwrap(), "0 755");
    // Cut short after the erase, the slot has no nestlocal but is whole.
    let (offset, _) = nand::slot_partition(Slot::Root0);
    let mut cut = image[offset / nand::PAGE * nand::PAGE_RAW..][..plan.raw.len()].to_vec();
    for &page in &plan.before {
        cut[page * nand::PAGE_RAW..(page + 1) * nand::PAGE_RAW].copy_from_slice(&plan.raw[page * nand::PAGE_RAW..(page + 1) * nand::PAGE_RAW]);
    }
    for &block in &plan.erase {
        let erased = &mut cut[block * nand::BLOCK_RAW..(block + 1) * nand::BLOCK_RAW];
        erased.fill(0xff);
        erased[PAGE + 2..PAGE + 10].copy_from_slice(&[0x85, 0x19, 0x03, 0x20, 0x08, 0, 0, 0]);
    }
    let mut cut = jffs2::Slot::load(cut).unwrap();
    assert_eq!(cut.read("nestlabs/sbin/nestlocal").unwrap(), None);
    assert_eq!(cut.read("etc/inittab").unwrap().unwrap(), inittab);
}

/// The whole patch, as a run applies it, on both slots: everything it writes
/// reads back from the programmed pages through a fresh scan.
#[test]
fn the_patch_applies_to_both_slots() {
    use unfr0zen_core::nestlocal::{generate_pki, Matter, Mqtt};
    use unfr0zen_core::patch::{patch_on_device, root_hash, Options};
    use unfr0zen_core::Device;
    let Some(image) = image() else { return };
    let object = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../build/custom-screen.o");
    let Ok(object) = std::fs::read(&object) else { panic!("{}: build it first (scripts/build-firmware.sh)", object.display()) };
    let pki = generate_pki().unwrap();
    let options = Options {
        root_hash: root_hash("unfr0zen").unwrap(),
        ntp_server: Some(String::new()),
        backplate_sim: true,
        block_cloud: true,
        mqtt: Some(Mqtt { url: "tcp://broker:1883".into(), ..Mqtt::default() }),
        ssh: true,
        ssh_server: Some(unfr0zen_core::nestlocal::Overlay { directories: vec![],
                                                             files: vec![("usr/sbin/dropbearmulti".into(), 0o755, b"DROPBEAR".to_vec())] }),
        matter: Some(Matter::from_pairing_code(unfr0zen_core::nestlocal::DEFAULT_MATTER_CODE).unwrap()),
        ..Options::default()
    };
    for which in [Slot::Root1, Slot::Root0] {
        let mut volume = slot(&image, which);
        let changes = patch_on_device(&mut volume, Device::J49, &options, b"NESTLOCAL", &object, &mut || Ok(pki.clone()),
                                      &Default::default(), false).unwrap_or_else(|e| panic!("{which}: {e}"));
        let (free, used) = volume.usage();
        let plan = volume.programmed().unwrap();
        eprintln!("{which}: {} changes, {} pages in {used} of {free} erased blocks", changes.len(), plan.before.len() + plan.after.len());
        let mut back = jffs2::Slot::load(plan.raw).unwrap();
        let inittab = String::from_utf8(back.read("etc/inittab").unwrap().unwrap().2).unwrap();
        assert!(!inittab.contains("ttyGS0"), "{inittab}");
        let client = String::from_utf8(back.read("etc/nestlabs/client.config").unwrap().unwrap().2).unwrap();
        assert!(client.contains("value=\"https://127.0.0.1/entry\""));
        assert_eq!(back.read("nestlabs/sbin/nestlocal").unwrap().unwrap().2, b"NESTLOCAL");
        assert_eq!(back.read("usr/sbin/dropbearmulti").unwrap().unwrap(), ("0:0".into(), 0o755, b"DROPBEAR".to_vec()));
        assert!(String::from_utf8(back.read("etc/init.d/rcS").unwrap().unwrap().2).unwrap()
            .contains("${INITDIR}/sshd start\n${INITDIR}/unfr0zen-ssh start\n"));
        assert!(String::from_utf8(back.read("etc/shadow").unwrap().unwrap().2).unwrap().contains("root:$6$"));
        let nlclient = back.read("nestlabs/sbin/nlclient").unwrap().unwrap().2;
        assert!(!back.read("etc/unfr0zen/nlclient.original").unwrap().unwrap().2.is_empty());
        assert_ne!(unfr0zen_core::shell::sha256_hex(&nlclient), "67596bb71d0225a3b54966e96cc19bc469c0e593ee72e37bfe96fc30a3741bb7");
        // A second run finds everything in place.
        let mut again = jffs2::Slot::load(volume.programmed().unwrap().raw).unwrap();
        let second = patch_on_device(&mut again, Device::J49, &options, b"NESTLOCAL", &object, &mut || Ok(pki.clone()),
                                     &Default::default(), false).unwrap();
        let rewritten: Vec<&str> = second.iter().map(|c| c.name.as_str()).collect();
        assert!(rewritten.iter().all(|name| *name == "etc/shadow" || *name == "etc/nestlocal/server.crt" || *name == "etc/nestlocal/server.key"), "{which}: {rewritten:?}");
    }
}
