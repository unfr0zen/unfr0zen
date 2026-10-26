#!/usr/bin/env python3
"""ARM radio scan/entropy contracts; SDK calls are mocked, RF is not simulated."""
import runpy
from pathlib import Path

from unicorn import UC_HOOK_CODE
from unicorn.arm_const import UC_ARM_REG_R0, UC_ARM_REG_R1, UC_ARM_REG_PC, UC_ARM_REG_LR

rcp = runpy.run_path(str(Path(__file__).with_name("test-source-match.py")))
cpu, call, symbols = rcp["cpu"], rcp["call"], rcp["symbols"]
now, rssi, entropy_status, threshold = 0, -90, 0, -75
events = []

def mock(cpu, address, size, user):
    global threshold
    name = user
    args = [cpu.reg_read(UC_ARM_REG_R0), cpu.reg_read(UC_ARM_REG_R1)]
    events.append((name, args))
    result = 0
    if name == "otPlatAlarmMicroGetNow": result = now
    elif name == "emRadioEnergyDetection": result = rssi & 0xFFFFFFFF
    elif name == "emRadioGetEdCcaThreshold": result = threshold & 0xFFFFFFFF
    elif name == "emRadioSetEdCcaThreshold": threshold = args[0]
    elif name == "emberGetStrongRandomNumberArray":
        assert args[1] == 1
        if entropy_status == 0: cpu.mem_write(args[0], b"\x34\x12")
        result = entropy_status
    cpu.reg_write(UC_ARM_REG_R0, result)
    cpu.reg_write(UC_ARM_REG_PC, cpu.reg_read(UC_ARM_REG_LR))

for name in ["otPlatAlarmMicroGetNow", "emRadioEnergyDetection", "emSetPhyRadioChannel",
             "emRadioWakeUp", "emRadioSleep", "emRadioEnableAutoAck", "otPlatRadioEnergyScanDone",
             "emberGetStrongRandomNumberArray", "emRadioSetEdCcaThreshold", "emRadioGetEdCcaThreshold"]:
    cpu.hook_add(UC_HOOK_CODE, mock, name, begin=symbols[name], end=symbols[name])

assert call("otPlatRadioEnergyScan", 0, 20, 1) != 0  # disabled radio
assert call("otPlatRadioEnable", 0) == 0
assert call("otPlatRadioGetRssi", 0) == 127  # sleeping radio
assert call("otPlatRadioReceive", 0, 11) == 0
assert call("otPlatRadioGetRssi", 0) & 0xFF == (-90 & 0xFF)
assert call("otPlatRadioEnergyScan", 0, 10, 1) == 7
assert call("otPlatRadioEnergyScan", 0, 20, 0) == 7
assert call("otPlatRadioEnergyScan", 0, 20, 1) == 0
assert call("otPlatRadioEnergyScan", 0, 21, 1) == 5
assert call("otPlatRadioSleep", 0) == 5
assert call("otPlatRadioDisable", 0) == 5
assert call("otPlatRadioReceive", 0, 21) == 5
assert call("otPlatRadioTransmit", 0, 0) == 5
events.clear()
now = 127
call("threadRadioEnergyScanTick")
assert not any(name == "emRadioEnergyDetection" for name, _ in events)
now, rssi = 128, -70
call("threadRadioEnergyScanTick")
now, rssi = 500, -42
call("threadRadioEnergyScanTick")
now, rssi = 1000, -80
call("threadRadioEnergyScanTick")
assert any(name == "emSetPhyRadioChannel" and args[0] == 11 for name, args in events)
done = [args for name, args in events if name == "otPlatRadioEnergyScanDone"]
assert len(done) == 1 and done[0][1] & 0xFF == (-42 & 0xFF)
assert call("otPlatRadioGetRssi", 0) & 0xFF == (-80 & 0xFF)  # receive restored
assert call("otPlatRadioSleep", 0) == 0
now = 0xFFFFFFC0
assert call("otPlatRadioEnergyScan", 0, 26, 1) == 0
now = (now + 1000) & 0xFFFFFFFF
call("threadRadioEnergyScanTick")
assert call("otPlatRadioGetRssi", 0) == 127  # sleep restored across timer wrap

scratch = 0x2001F000
assert call("otPlatRadioSetCcaEnergyDetectThreshold", 0, 127) == 7
assert call("otPlatRadioSetCcaEnergyDetectThreshold", 0, (-85 & 0xFFFFFFFF)) == 0
assert call("otPlatRadioGetCcaEnergyDetectThreshold", 0, scratch) == 0
assert cpu.mem_read(scratch, 1) == b"\xab"
assert call("otPlatRadioGetCcaEnergyDetectThreshold", 0, 0) == 7

cpu.mem_write(scratch, b"\xAA" * 8)
assert call("otPlatEntropyGet", scratch, 3) == 0
assert cpu.mem_read(scratch, 4) == b"\x34\x12\x34\xAA"
entropy_status = 0x7F
assert call("otPlatEntropyGet", scratch, 3) == 1
assert call("otPlatEntropyGet", 0, 3) == 7
identity = bytes(cpu.mem_read(symbols["rcp_identity"], 64)).decode()
assert identity == Path(rcp["sys"].argv[1]).with_name("radio.identity").read_text().strip()
print("ARM RSSI, asynchronous scan/restore, CCA, entropy failure and firmware identity tests passed")
