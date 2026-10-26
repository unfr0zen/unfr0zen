#!/usr/bin/env python3
"""Exercise the built ARM tables and SDK early-ACK ISR using Unicorn + pyelftools.

Usage: python test-source-match.py build/radio/radio.elf
The image boots as the Thread personality (radioZigbee is zero), which is the
one under test. RF timing and physical ACK transmission still require a device
and sniffer.
"""
import struct
import sys

from elftools.elf.elffile import ELFFile
from unicorn import Uc, UC_ARCH_ARM, UC_MODE_THUMB, UC_MODE_MCLASS, UC_HOOK_CODE
from unicorn.arm_const import UC_ARM_REG_R0, UC_ARM_REG_R1, UC_ARM_REG_R2, UC_ARM_REG_PC, UC_ARM_REG_SP, UC_ARM_REG_LR

with open(sys.argv[1], "rb") as firmware:
    elf = ELFFile(firmware)
    symbols = {s.name: s["st_value"] & ~1 if s["st_info"]["type"] == "STT_FUNC" else s["st_value"]
               for s in elf.get_section_by_name(".symtab").iter_symbols()}
    cpu = Uc(UC_ARCH_ARM, UC_MODE_THUMB | UC_MODE_MCLASS)
    for address, size in [(0x08000000, 0x80000), (0x20000000, 0x20000),
                          (0x40000000, 0x20000), (0xE000E000, 0x2000)]:
        cpu.mem_map(address, size)
    for segment in elf.iter_segments():
        if segment["p_type"] == "PT_LOAD":
            cpu.mem_write(segment["p_vaddr"], segment.data())

def word(address):
    return struct.unpack("<I", cpu.mem_read(address, 4))[0]

def put_word(address, value):
    cpu.mem_write(address, struct.pack("<I", value))

def skip_board_callback(cpu, address, size, user):
    cpu.reg_write(UC_ARM_REG_PC, cpu.reg_read(UC_ARM_REG_LR))

for name in ["halStackCancelSymbolDelay", "halPtaStackEvent"]:
    cpu.hook_add(UC_HOOK_CODE, skip_board_callback, begin=symbols[name], end=symbols[name])

def call(name, *args):
    cpu.reg_write(UC_ARM_REG_SP, 0x2001FFF0)
    cpu.reg_write(UC_ARM_REG_LR, 0x0807FFF1)
    for register, value in zip([UC_ARM_REG_R0, UC_ARM_REG_R1, UC_ARM_REG_R2], args):
        cpu.reg_write(register, value)
    cpu.emu_start(symbols[name] | 1, 0x0807FFF0, count=100000)
    assert cpu.reg_read(UC_ARM_REG_PC) == 0x0807FFF0, name
    return cpu.reg_read(UC_ARM_REG_R0)

# The Thread personality points the stack's child status table at its own.
put_word(symbols["emChildStatus"], word(symbols["threadRadioChildStatus"]))

def table_pending(address):
    index = call("threadRadioChildIndex", address)
    statuses = word(symbols["emChildStatus"])
    return bool(struct.unpack("<H", cpu.mem_read(statuses + index * 2, 2))[0] & 0x730)

scratch = 0x2001F000
def ext(value):
    cpu.mem_write(scratch, value.to_bytes(8, "little"))
    return scratch

# Error enum values come from the pinned OpenThread headers.
NONE, NO_BUFS, NO_ADDRESS = 0, 3, 10
call("otPlatRadioEnableSrcMatch", 0, 1)
assert not table_pending(0x1234)
assert not call("threadRadioLongIdPending", ext(0x0102030405060708))
for n in range(64):
    assert call("otPlatRadioAddSrcMatchShortEntry", 0, n) == NONE
    assert call("otPlatRadioAddSrcMatchExtEntry", 0, ext(n)) == NONE
assert call("otPlatRadioAddSrcMatchShortEntry", 0, 64) == NO_BUFS
assert call("otPlatRadioAddSrcMatchExtEntry", 0, ext(64)) == NO_BUFS
assert call("otPlatRadioAddSrcMatchShortEntry", 0, 63) == NONE
assert call("otPlatRadioAddSrcMatchExtEntry", 0, ext(63)) == NONE
assert call("otPlatRadioClearSrcMatchShortEntry", 0, 23) == NONE
assert call("otPlatRadioClearSrcMatchExtEntry", 0, ext(23)) == NONE
assert not table_pending(23) and table_pending(63)
assert not call("threadRadioLongIdPending", ext(23))
assert call("threadRadioLongIdPending", ext(63))
assert call("otPlatRadioClearSrcMatchShortEntry", 0, 23) == NO_ADDRESS
assert call("otPlatRadioClearSrcMatchExtEntry", 0, ext(23)) == NO_ADDRESS
assert call("otPlatRadioAddSrcMatchShortEntry", 0, 64) == NONE
assert call("otPlatRadioAddSrcMatchExtEntry", 0, ext(64)) == NONE
call("otPlatRadioClearSrcMatchShortEntries", 0)
assert not table_pending(64) and call("threadRadioLongIdPending", ext(64))
call("otPlatRadioClearSrcMatchExtEntries", 0)
assert not call("threadRadioLongIdPending", ext(64))
call("otPlatRadioAddSrcMatchShortEntry", 0, 0x1234)
call("otPlatRadioAddSrcMatchExtEntry", 0, ext(0x0102030405060708))

# The pinned SDK ISR loads its PHY register base with LDR.W r5,[pc,#imm12].
instruction = symbols["processRxAddressReceiveCompleteIsr"] + 10
first, second = struct.unpack("<HH", cpu.mem_read(instruction, 4))
assert first == 0xF8DF and second >> 12 == 5
phy = word(((instruction + 4) & ~3) + (second & 0xFFF))

def poll(source, extended=False, enabled=True, initial=False, buffer=0, command=4, ack=True, security=False):
    call("otPlatRadioEnableSrcMatch", 0, enabled)
    fcf = 0x0843 | (0xC000 if extended else 0x8000) | (0x20 if ack else 0) | (8 if security else 0)
    frame = struct.pack("<HBHH", fcf, 17, 0xAABB, 0x5678)
    frame += source.to_bytes(8 if extended else 2, "little") + bytes([command, 0, 0])
    cpu.mem_write(symbols["rxBuffer"] + buffer * 138, bytes([len(frame)]) + frame)
    put_word(phy + buffer * 4, 32)  # DMA halfwords already received
    put_word(phy + 0x64, 0x80)     # automatic ACK enabled
    put_word(phy + 0x48, 0xC00)    # both receive buffers allow pending override
    put_word(phy + 0x5C, 0x10 if initial else 0)
    put_word(phy + 0x44, int(initial))
    call("processRxAddressReceiveCompleteIsr", buffer)
    pending = bool(word(phy + 0x44) & 1)
    metadata = cpu.mem_read(symbols["framePendingSetInOutgoingAck"] + buffer, 1)[0]
    return pending, bool(metadata)

cases = 0
for buffer in [0, 1]:
    for initial in [False, True]:
        for extended, matched, absent in [(False, 0x1234, 0x4321),
                                          (True, 0x0102030405060708, 0x0807060504030201)]:
            for enabled in [False, True]:
                for source in [matched, absent]:
                    expected = not enabled or source == matched
                    assert poll(source, extended, enabled, initial, buffer) == (expected, expected)
                    cases += 1
            # Non-poll commands must clear pending, even with a matching source.
            assert poll(matched, extended, True, initial, buffer, command=1) == (False, False)
            cases += 1
            # Secured command identifiers are encrypted; source match still applies.
            assert poll(matched, extended, True, initial, buffer, command=0x9A, security=True) == (True, True)
            cases += 1
            assert poll(matched, extended, True, initial, buffer, ack=False) == (initial, False)
            cases += 1

# Exercise the final slot and an absent EUI-64 with seven matching prefix bytes.
call("otPlatRadioClearSrcMatchShortEntries", 0)
call("otPlatRadioClearSrcMatchExtEntries", 0)
for n in range(64):
    call("otPlatRadioAddSrcMatchShortEntry", 0, n)
    call("otPlatRadioAddSrcMatchExtEntry", 0, ext(0x0002030405060708 | (n << 56)))
assert poll(63) == (True, True)
assert poll(64) == (False, False)
assert poll(0x3F02030405060708, extended=True) == (True, True)
assert poll(0x4002030405060708, extended=True) == (False, False)
cases += 4

print(f"ARM source-match table tests and {cases} SDK early-ACK ISR cases passed")
