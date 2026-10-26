#!/usr/bin/env python3
"""Boot selection between the Zigbee NCP and the Thread RCP in the built image.

Usage: python test-personality.py build/radio/radio.elf
"""
import runpy
import struct
from pathlib import Path

from unicorn import UC_HOOK_CODE
from unicorn.arm_const import UC_ARM_REG_R0, UC_ARM_REG_PC, UC_ARM_REG_LR

rcp = runpy.run_path(str(Path(__file__).with_name("test-source-match.py")))
cpu, call, symbols, word, put_word = rcp["cpu"], rcp["call"], rcp["symbols"], rcp["word"], rcp["put_word"]
reached = []

def stop(cpu, address, size, user):
    reached.append(user)
    cpu.reg_write(UC_ARM_REG_R0, 0)
    cpu.reg_write(UC_ARM_REG_PC, cpu.reg_read(UC_ARM_REG_LR))

for name in ["zigbeeMain", "threadMain", "halReboot", "emberChildIndex", "emLongIdDataPendingForDevice"]:
    cpu.hook_add(UC_HOOK_CODE, stop, name, begin=symbols[name], end=symbols[name])

request = symbols["personalityRequest"]
assert symbols["__RESETINFO__begin"] <= request < symbols["__RESETINFO__end"]

# A cold boot, and one after a hardware reset, run the RCP.
put_word(request, 0)
call("main")
assert reached == ["threadMain"] and not cpu.mem_read(symbols["radioZigbee"], 1)[0]

# The RCP's selection reboots, and that one boot runs the NCP.
reached.clear()
call("radioSelectZigbee")
assert reached == ["halReboot"] and word(request) != 0
call("main")
assert reached == ["halReboot", "zigbeeMain"] and cpu.mem_read(symbols["radioZigbee"], 1)[0] == 1
assert word(request) == 0, "the request must not outlive the boot it selects"

# In the Zigbee personality the PHY's lookups reach the stack's child table.
reached.clear()
call("threadRadioChildIndex", 0x1234)
call("threadRadioLongIdPending", 0x2001F000)
assert reached == ["emberChildIndex", "emLongIdDataPendingForDevice"]

# The next boot is the RCP again.
reached.clear()
call("main")
assert reached == ["threadMain"]
print("personality selection and Zigbee lookup forwarding tests passed")
