//! The gen 1's and gen 2's loopback-only RCS input, restricted to the
//! temperature command.
use super::elf::{align, bytes, half, word, Elf};
use crate::{device, shell::sha256_hex, Result};

const STOCK_SHA: &str = "67596bb71d0225a3b54966e96cc19bc469c0e593ee72e37bfe96fc30a3741bb7";
const HOOK: u32 = 0x47e88;
const COMMAND_TABLE: u32 = 0x4ef920;
const HANDLER: u32 = 0x14173c;
const COMMAND_COUNT: u32 = 0x141bc4;
const RUN_FLAG: u32 = 0x141c74;
const READ_CALL: u32 = 0x141d40;
const OPEN_CALL: u32 = 0x141c7c;
const SENSOR_MENU: u32 = 0x14282c;
const CONFIGURE: u32 = 0x19a668;
const POST_READING: u32 = 0x141858;
const POST_REPLY: u32 = 0x14185c;
const SETTINGS_ENTRY: u32 = 0x11474c;
const MOCK_SENSORS: u32 = 0x47e94;
const TEMPERATURE_CALL: u32 = 0x237af4;
const TEMPERATURE_RESULT: u32 = 0x237afc;
const OPERATOR_NEW: u32 = 0x2ad84;
const CONSTRUCTOR: u32 = 0x141b2c;
const CREATE_THREAD: u32 = 0x2bf24;
const COMMAND: &[u8] = b"kryptonitedata\0";
const RETRY_REPLY: &[u8] = b"Retry\0";

fn branch(from: u32, to: u32, opcode: u32) -> Result<u32> {
    let delta = i64::from(to) - i64::from(from) - 8;
    if delta % 4 != 0 || !(-(1 << 25)..(1 << 25)).contains(&delta) {
        return device("external temperature branch out of range");
    }
    Ok(opcode | ((delta >> 2) as u32 & 0xffffff))
}

/// Called at client initialization with r4 = client. Its existing teardown
/// stops/joins the thread at +0xa8 and deletes the object at +0x3d4.
fn startup(address: u32) -> Result<Vec<u8>> {
    let mut code = vec![
        0xe92d500f, // push {r0-r3,r12,lr}
        0xe24dd010, // sub sp,sp,#16 (NSPR's three stack arguments, aligned)
        0xe59403d4, // ldr r0,[r4,#0x3d4]
        0xe3500000, // cmp r0,#0
        0,          // bne done
        0xe3a00004, // mov r0,#4 (testaccess's run flag)
        0,          // bl operator new
        0xe3500000, // cmp r0,#0
        0,          // beq done
        0xe58403d4, // str r0,[r4,#0x3d4]
        0xe5941070, // ldr r1,[r4,#0x70] (globals, not client)
        0,          // bl testaccess constructor
        0xe59423d4, // ldr r2,[r4,#0x3d4]
        0xe3a00001, // mov r0,#1
        0xe5820000, // str r0,[r2] (run flag set before the thread can start)
        0xe3a00000, // mov r0,#0 (PR_USER_THREAD)
        0xe3011e7c, // movw r1,#0x1e7c
        0xe3401014, // movt r1,#0x14 (thread entry)
        0xe3a03001, // mov r3,#1 (PR_PRIORITY_NORMAL)
        0xe58d3000, // str r3,[sp] (PR_GLOBAL_THREAD)
        0xe3a0c000, // mov r12,#0
        0xe58dc004, // str r12,[sp,#4] (PR_JOINABLE_THREAD)
        0xe58dc008, // str r12,[sp,#8] (default stack size)
        0,          // bl PR_CreateThread
        0xe58400a8, // str r0,[r4,#0xa8]
        0xe28dd010, // done: add sp,sp,#16
        0xe8bd500f, // pop {r0-r3,r12,lr}
        0xe3051140, // displaced movw r1,#0x5140
        0,          // b hook+4
    ];
    for (index, target, opcode) in [
        (4, address + 25 * 4, 0x1a000000),
        (6, OPERATOR_NEW, 0xeb000000),
        (8, address + 25 * 4, 0x0a000000),
        (11, CONSTRUCTOR, 0xeb000000),
        (23, CREATE_THREAD, 0xeb000000),
        (28, HOOK + 4, 0xea000000),
    ] {
        code[index] = branch(address + index as u32 * 4, target, opcode)?;
    }
    Ok(code.into_iter().flat_map(u32::to_le_bytes).collect())
}

/// Pass the native run flag and reply string to the bounded command reader.
fn bounded_read(address: u32, reader: u32) -> Result<Vec<u8>> {
    Ok([0xe59fc00c, 0xe92d5000, 0xe1a03008, branch(address + 12, reader, 0xeb000000)?, 0xe8bd9000, 0x4fa9c0]
        .into_iter()
        .flat_map(u32::to_le_bytes)
        .collect())
}

/// Set SO_REUSEADDR before binding, so accepted connections in TIME_WAIT do
/// not prevent the next nlclient process from starting its listener.
fn reusable_socket(address: u32) -> Result<Vec<u8>> {
    let mut code = vec![
        0xe92d4010, // push {r4,lr}
        0xe24dd010, // sub sp,sp,#16
        0,          // bl PR_OpenTCPSocket
        0xe1b04000, // movs r4,r0
        0,          // beq done
        0,          // bl PR_FileDesc2NativeHandle
        0xe3a01001, // mov r1,#1 (SOL_SOCKET)
        0xe3a02002, // mov r2,#2 (SO_REUSEADDR)
        0xe3a03001, // mov r3,#1
        0xe58d3004, // str r3,[sp,#4] (optval)
        0xe3a03004, // mov r3,#4
        0xe58d3000, // str r3,[sp] (optlen, fifth argument)
        0xe28d3004, // add r3,sp,#4
        0,          // bl setsockopt
        0xe1a00004, // mov r0,r4
        0xe28dd010, // done: add sp,sp,#16
        0xe8bd8010, // pop {r4,pc}
    ];
    for (index, target, opcode) in [
        (2, 0x29e3c, 0xeb000000),
        (4, address + 15 * 4, 0x0a000000),
        (5, 0x2c608, 0xeb000000),
        (13, 0x2cbfc, 0xeb000000),
    ] {
        code[index] = branch(address + index as u32 * 4, target, opcode)?;
    }
    Ok(code.into_iter().flat_map(u32::to_le_bytes).collect())
}

/// Return temperature and validity separately: 0 °C is a valid sample, not
/// the native caller's missing-temperature sentinel. Preserve error handling.
fn control_reading(address: u32) -> Result<Vec<u8>> {
    let mut code = vec![
        0xe92d4010, 0xe24dd008, 0xe1a04000, 0xe5900018, 0xe3500000, 0, 0xe1a0100d, 0, 0xe3500000, 0, 0xe59d0000,
        0xe3a01001, 0, 0xe1a00004, 0, 0xe3a01000, 0xe28dd008, 0xe8bd4010, 0xe12fff1e,
    ];
    for (index, target, opcode) in [
        (5, address + 13 * 4, 0x0a000000),
        (7, 0x1ebc18, 0xeb000000),
        (9, address + 13 * 4, 0x1a000000),
        (12, address + 16 * 4, 0xea000000),
        (14, 0x1b72ec, 0xeb000000),
    ] {
        code[index] = branch(address + index as u32 * 4, target, opcode)?;
    }
    Ok(code.into_iter().flat_map(u32::to_le_bytes).collect())
}

fn control_result(address: u32) -> Result<Vec<u8>> {
    Ok([0xe3510000, branch(address + 4, 0x237a8c, 0x0a000000)?, branch(address + 8, 0x237ad8, 0xea000000)?]
        .into_iter()
        .flat_map(u32::to_le_bytes)
        .collect())
}

fn screen_guard(address: u32) -> Result<Vec<u8>> {
    Ok([0xe3500000, 0x012fff1e, 0xe59000ec, branch(address + 12, 0x19a66c, 0xea000000)?]
        .into_iter()
        .flat_map(u32::to_le_bytes)
        .collect())
}

fn sample_reply(address: u32, retry: u32) -> Result<Vec<u8>> {
    Ok([0xe3500000, 0x159f1008, 0x059f1008, 0xe2840004, branch(address + 16, 0x141868, 0xea000000)?, 0x3dfa1c, retry]
        .into_iter()
        .flat_map(u32::to_le_bytes)
        .collect())
}

fn screen_bindings(elf: &Elf) -> Result<std::collections::BTreeMap<String, u32>> {
    let mut bindings: std::collections::BTreeMap<_, _> = [
        ("rcs_screen_lookup", 0x51a1c),
        ("rcs_layer_lookup", 0x519a0),
        ("rcs_layer_new", 0x19862c),
        ("rcs_screen_new", 0x19a6f8),
        ("rcs_add_layer", 0x51968),
        ("rcs_add_screen", 0x519e8),
        ("rcs_associated", 0x96a08),
        ("rcs_native_post", 0x6c180),
    ]
    .into_iter()
    .map(|(name, address)| (name.into(), address))
    .collect();
    let imports = super::discovery::imported(elf)?;
    for (name, symbol) in [
        ("strcmp", "strcmp"),
        ("rcs_string_find", "_ZNK8nlString4FindERKS_6nlBOOL"),
        ("rcs_native_read", "PR_Read"),
        ("rcs_interval", "PR_MillisecondsToInterval"),
        ("rcs_string_clear", "_ZN8nlString5ClearEv"),
        ("rcs_poll", "PR_Poll"),
    ] {
        let address =
            imports.get(symbol).ok_or_else(|| crate::Error::Device(format!("missing RCS import {symbol}")))?;
        bindings.insert(name.into(), *address);
    }
    Ok(bindings)
}

/// Extend the screen builder's executable load segment. The stock hash guards
/// the fixed native addresses and client layout; unknown builds fail closed.
/// The caller includes these edits in the same reversible screen journal.
pub fn patch(stock: &[u8], output: &mut Vec<u8>, screen_object: &[u8]) -> Result<()> {
    if sha256_hex(stock) != STOCK_SHA {
        return device("the external temperature requires the reviewed nlclient 5.9.4-5 binary");
    }
    extend(stock, output, screen_object)
}

fn extend(stock: &[u8], output: &mut Vec<u8>, screen_object: &[u8]) -> Result<()> {
    let elf = Elf::parse(stock)?;
    let hook = elf.offset(HOOK)?;
    let table = elf.offset(COMMAND_TABLE)?;
    let limit = elf.offset(COMMAND_COUNT)?;
    let run_flag = elf.offset(RUN_FLAG)?;
    let read = elf.offset(READ_CALL)?;
    let open = elf.offset(OPEN_CALL)?;
    let menu = elf.offset(SENSOR_MENU)?;
    let configure = elf.offset(CONFIGURE)?;
    let post = elf.offset(POST_READING)?;
    let reply = elf.offset(POST_REPLY)?;
    let visible = elf.offset(SETTINGS_ENTRY)?;
    let simulation = elf.offset(MOCK_SENSORS)?;
    let temperature = elf.offset(TEMPERATURE_CALL)?;
    let temperature_result = elf.offset(TEMPERATURE_RESULT)?;
    for (offset, expected) in [
        (hook, 0xe3051140),
        (limit, 0xe3560068),
        (run_flag, 0xe5882000),
        (read, 0xebfbaa8d),
        (open, 0xebfba06e),
        (menu, 0xebfc3c7a),
        (configure, 0xe59000ec),
        (post, 0xebfcaa48),
        (reply, 0xe30f1a1c),
        (visible, 0x0affffb0),
        (simulation, 0xebff8824),
        (temperature, 0xebfdfdfc),
        (temperature_result, 0xeaffffe0),
    ] {
        if word(output, offset)? != expected {
            return device("the external temperature patch site changed");
        }
    }
    if bytes(output, table, 12)? != bytes(stock, table, 12)? {
        return device("the external temperature command table changed");
    }
    let phoff = word(output, 28)? as usize;
    let stride = half(output, 42)? as usize;
    if stride != 32 {
        return device("unsupported nlclient program header size");
    }
    let mut segment = None;
    for i in 0..half(output, 44)? as usize {
        let at = phoff + i * stride;
        let offset = word(output, at + 4)? as usize;
        let size = word(output, at + 16)? as usize;
        if word(output, at)? == 1
            && word(output, at + 24)? == 5
            && offset >= stock.len()
            && offset.checked_add(size) == Some(output.len())
            && word(output, at + 20)? as usize == size
        {
            if segment.is_some() {
                return device("ambiguous nlclient payload segment");
            }
            segment = Some((at, offset, word(output, at + 8)?));
        }
    }
    let Some((header, offset, base)) = segment else {
        return device("the external temperature needs the linked screen payload");
    };
    let start = align(output.len(), 4)?;
    let address = base
        .checked_add((start - offset) as u32)
        .ok_or_else(|| crate::Error::Device("external temperature payload address overflow".into()))?;
    let mut code = startup(address)?;
    let read_address = address + code.len() as u32;
    code.extend(bounded_read(read_address, read_address)?);
    let socket_address = address + code.len() as u32;
    code.extend(reusable_socket(socket_address)?);
    let temperature_address = address + code.len() as u32;
    code.extend(control_reading(temperature_address)?);
    let result_address = address + code.len() as u32;
    code.extend(control_result(result_address)?);
    let guard_address = address + code.len() as u32;
    code.extend(screen_guard(guard_address)?);
    let reply_address = address + code.len() as u32;
    code.extend(sample_reply(reply_address, reply_address + 28 + COMMAND.len() as u32)?);
    let command = address + code.len() as u32;
    code.extend(COMMAND);
    code.extend(RETRY_REPLY);
    code.resize(align(code.len(), 4)?, 0);
    let screen = super::linker::link(screen_object, address + code.len() as u32, &screen_bindings(&elf)?)?;
    let post_entry = screen
        .symbols
        .get("rcs_post_reading")
        .copied()
        .ok_or_else(|| crate::Error::Device("the RCS payload has no reading gate".into()))?;
    let reader = screen
        .symbols
        .get("rcs_read_command")
        .copied()
        .ok_or_else(|| crate::Error::Device("the RCS payload has no command reader".into()))?;
    let read_code = bounded_read(read_address, reader)?;
    let read_at = (read_address - address) as usize;
    code[read_at..read_at + read_code.len()].copy_from_slice(&read_code);
    code.extend(screen.bytes);
    let jump = branch(HOOK, address, 0xea000000)?;
    let read_jump = branch(READ_CALL, read_address, 0xeb000000)?;
    let socket_jump = branch(OPEN_CALL, socket_address, 0xeb000000)?;
    let menu_jump = branch(SENSOR_MENU, screen.entry, 0xeb000000)?;
    let guard_jump = branch(CONFIGURE, guard_address, 0xea000000)?;
    let post_jump = branch(POST_READING, post_entry, 0xeb000000)?;
    let reply_jump = branch(POST_REPLY, reply_address, 0xea000000)?;
    let temperature_jump = branch(TEMPERATURE_CALL, temperature_address, 0xeb000000)?;
    let result_jump = branch(TEMPERATURE_RESULT, result_address, 0xea000000)?;
    output.resize(start, 0);
    output.extend(code);
    let size = u32::try_from(output.len() - offset)
        .map_err(|_| crate::Error::Device("external temperature payload size overflow".into()))?;
    for at in [header + 16, header + 20] {
        output[at..at + 4].copy_from_slice(&size.to_le_bytes());
    }
    output[hook..hook + 4].copy_from_slice(&jump.to_le_bytes());
    output[read..read + 4].copy_from_slice(&read_jump.to_le_bytes());
    output[open..open + 4].copy_from_slice(&socket_jump.to_le_bytes());
    output[menu..menu + 4].copy_from_slice(&menu_jump.to_le_bytes());
    output[configure..configure + 4].copy_from_slice(&guard_jump.to_le_bytes());
    output[post..post + 4].copy_from_slice(&post_jump.to_le_bytes());
    output[reply..reply + 4].copy_from_slice(&reply_jump.to_le_bytes());
    // The main menu already offers Active Sensor. Skip the duplicate
    // schedule entry in the Settings carousel.
    output[visible..visible + 4].copy_from_slice(&0xeaffffbeu32.to_le_bytes());
    // Only the cloud-associated virtual sensor is used, never the firmware's
    // fixed Bedroom/Hallway test devices, even on a simulated unit.
    output[simulation..simulation + 4].copy_from_slice(&0xe3a00000u32.to_le_bytes());
    output[temperature..temperature + 4].copy_from_slice(&temperature_jump.to_le_bytes());
    output[temperature_result..temperature_result + 4].copy_from_slice(&result_jump.to_le_bytes());
    // The worker must not overwrite a stop request made before it runs.
    output[run_flag..run_flag + 4].copy_from_slice(&0xe1a00000u32.to_le_bytes());
    // Only this command is searched. None of the stock diagnostic handlers
    // (including reset and HVAC controls) can be reached through the server.
    output[limit..limit + 4].copy_from_slice(&0xe3560001u32.to_le_bytes());
    output[table..table + 4].copy_from_slice(&command.to_le_bytes());
    output[table + 4..table + 8].copy_from_slice(&HANDLER.to_le_bytes());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_firmware_is_rejected_without_changing_output() {
        let mut output = b"linked screen".to_vec();
        assert!(patch(b"unknown firmware", &mut output, b"").is_err());
        assert_eq!(output, b"linked screen");
    }

    #[test]
    fn startup_replays_the_hook_and_branches_back() {
        let address = 0x520000;
        let code = startup(address).unwrap();
        assert_eq!(word(&code, 0).unwrap(), 0xe92d500f);
        assert_eq!(word(&code, 26 * 4).unwrap(), 0xe8bd500f);
        assert_eq!(word(&code, 27 * 4).unwrap(), 0xe3051140);
        assert_eq!(word(&code, 28 * 4).unwrap(), branch(address + 28 * 4, HOOK + 4, 0xea000000).unwrap());
    }

    #[test]
    fn machine_instructions_match_the_arm_assembler() {
        let tmp = tempfile::tempdir().unwrap();
        let object = tmp.path().join("reference.o");
        let source = std::path::Path::new(env!("UNFR0ZEN_ROOT"))
            .join("platform/common/nlclient/tests/external-temperature/j49-5.9.4-5.S");
        let compiled = std::process::Command::new("clang")
            .args(["--target=arm-none-eabi", "-march=armv7-a", "-c"])
            .arg(source)
            .arg("-o")
            .arg(&object)
            .output();
        let compiled = match compiled {
            Err(e)
                if e.kind() == std::io::ErrorKind::NotFound
                    && std::env::var_os("NLCLIENT_REQUIRE_FIRMWARE").is_none() =>
            {
                return
            }
            result => result.unwrap(),
        };
        assert!(compiled.status.success(), "{}", String::from_utf8_lossy(&compiled.stderr));
        let object = std::fs::read(object).unwrap();
        let elf = Elf::parse(&object).unwrap();
        let text = elf.section(".text").unwrap();
        let symtab = elf.section(".symtab").unwrap();
        let strings = elf.contents(&elf.sections[symtab.link]);
        let base = 0x520000;
        let mut labels = std::collections::BTreeMap::new();
        for at in (symtab.offset..symtab.offset + symtab.size).step_by(16) {
            if half(&object, at + 14).unwrap() as usize != elf.section(".rel.text").unwrap().info {
                continue;
            }
            let name = super::super::elf::string(strings, word(&object, at).unwrap() as usize).unwrap();
            labels.insert(name, base + word(&object, at + 4).unwrap());
        }
        let mut emitted = startup(base).unwrap();
        for (name, code) in [
            ("bounded_read", bounded_read(labels["bounded_read"], base).unwrap()),
            ("reusable_socket", reusable_socket(labels["reusable_socket"]).unwrap()),
            ("control_reading", control_reading(labels["control_reading"]).unwrap()),
            ("control_result", control_result(labels["control_result"]).unwrap()),
            ("screen_guard", screen_guard(labels["screen_guard"]).unwrap()),
            ("sample_reply", sample_reply(labels["sample_reply"], labels["retry_reply"]).unwrap()),
        ] {
            assert_eq!(base + emitted.len() as u32, labels[name], "{name}");
            emitted.extend(code);
        }
        assert_eq!(base + emitted.len() as u32, labels["command_name"]);
        emitted.extend(COMMAND);
        emitted.extend(RETRY_REPLY);
        emitted.resize(align(emitted.len(), 4).unwrap(), 0);
        let mut expected = elf.contents(text).to_vec();
        // A relocatable assembly supplies the instructions and local branches.
        // Externally linked operands are checked on the actual mapped stock patch.
        let rel = elf.section(".rel.text").unwrap();
        for at in (rel.offset..rel.offset + rel.size).step_by(8) {
            let offset = word(&object, at).unwrap() as usize;
            let width = match word(&object, at + 4).unwrap() & 255 {
                2 => 4,       // ABS32
                28 | 29 => 3, // CALL/JUMP24 immediate, preserving condition and opcode
                kind => panic!("unexpected assembler relocation {kind}"),
            };
            expected[offset..offset + width].fill(0);
            emitted[offset..offset + width].fill(0);
        }
        assert_eq!(emitted, expected);
    }

    #[test]
    fn branches_reject_unaligned_or_unreachable_targets() {
        assert!(branch(0x1000, 0x1001, 0xea000000).is_err());
        assert!(branch(0x1000, 0x4000000, 0xea000000).is_err());
    }
}

#[cfg(test)]
mod firmware_tests;
