#!/bin/sh
# Builds OUT/nand-flasher.bin, the j49's NAND reader and programmer, from
# nand-flasher.c: freestanding ARMv7 code linked at 0x80100000, its entry
# first. Nothing zeroes .bss or copies .data on the unit, so it may have
# neither. Needs arm-none-eabi-gcc and its binutils.
#
#     platform/j49/build.sh OUT
set -eu

[ $# -eq 1 ] || { echo "usage: $0 OUT" >&2; exit 2; }
dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
mkdir -p "$1"
out=$(CDPATH= cd -- "$1" && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

arm-none-eabi-gcc -march=armv7-a -marm -Os -Wall -Werror -ffreestanding -fno-builtin -fno-toplevel-reorder \
    -fno-unwind-tables -fno-asynchronous-unwind-tables -nostdlib -c -o "$work/nand-flasher.o" "$dir/nand-flasher.c"
arm-none-eabi-ld -Ttext 0x80100000 -e _start -o "$work/nand-flasher.elf" "$work/nand-flasher.o"
if arm-none-eabi-objdump -h "$work/nand-flasher.elf" | grep -E '\.(data|bss)' | grep -v ' 00000000 '; then
    echo "nand-flasher has initialized or zeroed data" >&2
    exit 1
fi
test "$(arm-none-eabi-nm "$work/nand-flasher.elf" | sed -n 's/ T _start$//p')" = 80100000
arm-none-eabi-objcopy -O binary -j .text -j .rodata "$work/nand-flasher.elf" "$out/nand-flasher.bin"
echo "Built $out/nand-flasher.bin ($(wc -c < "$out/nand-flasher.bin") bytes)"
