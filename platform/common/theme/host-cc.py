#!/usr/bin/env python3
"""Build LuaJIT's 32-bit host tools on an AArch64 build machine."""

import os
import shlex
import subprocess
import sys

args = sys.argv[1:]
subprocess.run(["python3", "-m", "ziglang", "cc", "-target", "x86-linux-musl", "-static", *args], check=True)
if "-o" in args and not any(flag in args for flag in ["-c", "-E", "-S"]):
    output = os.path.abspath(args[args.index("-o") + 1])
    os.replace(output, output + ".elf")
    with open(output, "w") as script:
        script.write("#!/bin/sh\nexec qemu-i386 " + shlex.quote(output + ".elf") + ' "$@"\n')
    os.chmod(output, 0o755)
