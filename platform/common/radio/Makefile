# Firmware for the Nest's radio, built with GCC. On the gen3's EM3581
# (PART=em3581), one image that boots as an OpenThread RCP (thread/rcp.c) and
# reboots into an EmberZNet NCP-SPI on request (personality.c). The gen2's
# EM357 (PART=em357) holds one at a time: PERSONALITY=zigbee is the NCP alone,
# PERSONALITY=thread the RCP alone.
#
# Silicon Labs ships the EM3581 Zigbee stack only as IAR-built libraries; they
# are standard AEABI objects, so GCC links them. What AppBuilder would generate
# (the EZSP frame dispatcher, the plugin glue) is gen-dispatch.py and ncp.c.
# OpenThread's radio-side libraries come from thread/build.sh's CMake build.
#
#   SDK       Gecko SDK 2.6 (EmberZNet 6.6): the stack libraries and HAL sources
#   GSDK      gecko_sdk 4.5: EM358x CMSIS startup, platform/Device and platform/CMSIS
#   OT        the pinned OpenThread checkout (thread/fetch-openthread.sh)
#   OT_BUILD  its radio build directory (openthread-rcp and its libraries)
#
# Output: $(OUT)/radio.bin, an image for zb-loader --app-easyload (loads at
# 0x08002000, as Nest's own ip-modem-app.bin does), and $(OUT)/radio.identity,
# the SHA-256 of everything linked into it, which the image also carries.

# The image's version: nestlocal installs its own image on a radio that runs
# another (it reads it back with EZSP's getXncpInfo). Bump with every change.
VERSION = 3

SDK   ?= $(error set SDK to a Gecko SDK 2.6 tree)
GSDK  ?= $(error set GSDK to a gecko_sdk 4.5 tree)
OT    ?= ../.work/openthread
OT_BUILD ?= ../.work/openthread-radio
OUT   ?= ../build/radio

CROSS ?= arm-none-eabi-
CC     = $(CROSS)gcc
OBJCOPY = $(CROSS)objcopy

PART ?= em3581
# both (the EM3581's), zigbee or thread.
PERSONALITY ?= both
# Whether the image has the EmberZNet NCP.
NCP = $(filter-out thread,$(PERSONALITY))
LIB  = $(SDK)/protocol/zigbee/build
HAL  = $(SDK)/platform/base/hal
ZB   = $(SDK)/protocol/zigbee

ifeq ($(PART),em357)
FAMILY = em35x
# Nest's set-up (wpantund.conf, ConnectIP's mode 5): boost, the
# bidirectional port into the board's amplifier, at most -10 dBm to spare the
# WiFi front end.
MICRO  = -DCORTEXM3_EM357 -DEZSP_SPI_PORT=2 -DRADIO_TX_POWER_MODE=EMBER_TX_POWER_MODE_BOOST -DRADIO_TX_POWER_MAX=-10
# The gen2's loader takes the image from 0x08002000, its address table first.
PREFIX = 0
# The SDK's SC2 pins are the gen2's.
SPI_PATCH =
else
FAMILY = em358x
# Nest's set-up (wpantund.conf): the alternate port, into the amplifier.
MICRO  = -DCORTEXM3_EM3581 -DEZSP_SPI_PORT=1 -DRADIO_TX_POWER_MODE=EMBER_TX_POWER_MODE_ALTERNATE
# The gen3's loader takes the image from 0x08002000; its bootloader keeps
# 16 KiB, so 8 KiB of zeros come first, then the address table.
PREFIX = 8192
SPI_PATCH = $(PART)/nest-spi.patch
endif
# Each part's board header, linker script and SPI pin patch are in its directory.
BOARD  = $(PART)/board.h
LINK   = $(PART)/link.ld
DEV = $(GSDK)/platform/Device/SiliconLabs/$(FAMILY)

# The stack, the NCP core and the EZSP command handlers, prebuilt. A router
# needs PRO, security and binding; Green Power, ZLL, CBKE and mfglib are stubs.
LIBS = em260-spi-util-library em260-concentrator-stub-library \
       ncp-pro-library ncp-security-library ncp-binding-library \
       zigbee-pro-stack zigbee-r22-support-library security-library-core \
       security-library-link-keys install-code-library packet-validate-library \
       binding-table-library end-device-bind-stub-library debug-basic-stub-library \
       debug-extended-stub-library multi-network-stub-library gp-stub-library \
       zll-stub-library mfglib-stub-library cbke-stub-library cbke-163k1-stub-library \
       cbke-283k1-stub-library cbke-library-dsa-sign-stub cbke-library-dsa-verify-stub \
       cbke-library-dsa-verify-283k1-stub sim-eeprom1-library hal-library
ifeq ($(PERSONALITY),thread)
# The RCP alone takes no EZSP: the NCP's libraries go, stubs in their place.
LIBS := $(filter-out em260-spi-util-library em260-concentrator-stub-library ncp-pro-library \
          ncp-security-library ncp-binding-library,$(LIBS))
endif
LIBFILES = $(foreach l,$(LIBS),$(LIB)/$(l)-cortexm3-iar-$(PART)-em3xx/$(l).a)

SRCS = \
  $(HAL)/micro/cortexm3/cstartup-common.c \
  $(HAL)/micro/cortexm3/assert-crash-handlers.c \
  $(HAL)/micro/cortexm3/bootloader-interface.c \
  $(HAL)/micro/cortexm3/bootloader-interface-standalone.c \
  $(HAL)/micro/cortexm3/micro.c \
  $(HAL)/micro/cortexm3/micro-common.c \
  $(HAL)/micro/cortexm3/mfg-token.c \
  $(HAL)/micro/cortexm3/token.c \
  $(HAL)/micro/cortexm3/mpu.c \
  $(HAL)/micro/cortexm3/sleep.c \
  $(HAL)/micro/generic/crc.c \
  $(HAL)/micro/generic/endian.c \
  $(HAL)/micro/generic/mem-util.c \
  $(HAL)/micro/generic/random.c \
  $(HAL)/micro/generic/token-def.c \
  $(HAL)/plugin/adc/adc-em3xx.c \
  $(HAL)/plugin/antenna-stub/antenna-stub.c \
  $(HAL)/plugin/debug-jtag-stub/debug-jtag-stub.c \
  $(HAL)/plugin/sim-eeprom1/sim-eeprom.c \
  $(ZB)/stack/config/ember-configuration.c \
  $(ZB)/stack/framework/ccm-star.c \
  $(ZB)/stack/framework/strong-random-api.c \
  $(if $(NCP),$(ZB)/app/em260/em260-common.c) \
  $(DEV)/Source/system_$(FAMILY).c \
  $(DEV)/Source/GCC/startup_$(FAMILY).c \
  $(HAL)/micro/cortexm3/spi-protocol.c \
  $(if $(NCP),$(OUT)/dispatch.c ncp.c) \
  runtime.c
ifneq ($(PERSONALITY),zigbee)
SRCS += personality.c thread/rcp.c thread/source-match.c
endif
ifeq ($(PERSONALITY),thread)
# The stack's packet buffers serve only the PHY's receive path; the RCP's
# own queue takes each frame.
THREAD_DEFS = -DEMBER_PACKET_BUFFER_COUNT=24
SRCS += thread/libc.c thread/stack-stubs.c
endif
ASMS = \
  $(HAL)/micro/cortexm3/faults.s79 \
  $(HAL)/micro/cortexm3/isr-stubs.s79 \
  $(DEV)/Source/GCC/irqToIsrTrampolines_$(FAMILY).s

# The SPI driver's pin header, with Nest's pins where the SDK's differ, in
# $(OUT)/sdk: first on the include path, so spi-protocol.c picks it up.
PATCHED = hal/micro/cortexm3/ezsp-spi-serial-controller-autoselect.h

DEFS = -DPLATFORM_HEADER='"hal/micro/cortexm3/compiler/gcc.h"' \
       -DBOARD_HEADER='"$(BOARD)"' -DCONFIGURATION_HEADER='"ncp-config.h"' \
       -DCORTEXM3 -DCORTEXM3_EMBER_MICRO $(MICRO) -DEMBER_STACK_ZIGBEE \
       -DPHY_EM3XX -DHAL_MICRO -DEZSP_SPI -DEMBER_AF_NCP -D__NCP_CONFIG__ \
       -DNVIC_CONFIG='"hal/micro/cortexm3/nvic-config.h"' -D__START=main \
       -D__STACK_SIZE=0 -D__HEAP_SIZE=0 -D__STARTUP_CLEAR_BSS -DCUSTOMER_APPLICATION_VERSION=$(VERSION) \
       -DOPENTHREAD_RADIO=1 -DOPENTHREAD_FTD=0 -DOPENTHREAD_MTD=0 -DRADIO_PERSONALITY_$(PERSONALITY) $(THREAD_DEFS)
INCS = -I. -I$(OUT)/sdk -I$(OUT) -I$(HAL)/.. -I$(HAL) -I$(HAL)/micro/cortexm3/em35x \
       -I$(HAL)/plugin -I$(ZB) -I$(ZB)/stack -I$(ZB)/app/util -I$(DEV)/Include -I$(GSDK)/platform/CMSIS/Core/Include \
       -I$(OT)/include
# The same image wherever the trees are: asserts name their files.
PREFIXES = -ffile-prefix-map=$(abspath $(SDK))/=sdk/ -ffile-prefix-map=$(abspath $(GSDK))/=gsdk/ \
           -ffile-prefix-map=$(abspath $(OUT))/=out/ -ffile-prefix-map=$(CURDIR)/=radio/
CFLAGS = -mcpu=cortex-m3 -mthumb -Os -std=gnu99 -ffunction-sections -fdata-sections \
         -fno-tree-loop-distribute-patterns -Wall -Wno-unused $(PREFIXES) $(DEFS) $(INCS)
LDFLAGS = -mcpu=cortex-m3 -mthumb -nostdlib -Wl,--wrap=SystemInit \
          -Wl,--wrap=emberIncomingManyToOneRouteRequestHandler -Wl,--gc-sections \
          -L$(HAL)/micro/cortexm3 -T $(LINK) -Wl,-Map=$(OUT)/radio.map
ifneq ($(PERSONALITY),zigbee)
# The RCP takes the PHY's receive and transmit-complete callbacks and the
# assert from the stack; the Zigbee personality passes them on.
LDFLAGS += -Wl,--wrap=emRadioTransmitCompleteCallback -Wl,--wrap=emPacketReceivedInternalIsrCallback \
           -Wl,--wrap=halInternalAssertFailed
OT_LIBFILES = $(shell find $(OT_BUILD) -name '*.a')
endif

# The IRQ trampolines first: they and the startup both define the vector
# table's handlers weak (the startup as Default_Handler), and the first wins.
OBJS = $(patsubst %,$(OUT)/obj/%.o,$(notdir $(ASMS) $(SRCS)))
# Library objects with symbols renamed, linked ahead of their archives: with
# both personalities the em260 main is the Zigbee one's; with Thread the PHY's
# lookups go through its source matching.
ifeq ($(PERSONALITY),both)
OBJS += $(OUT)/sdk/zigbee-main.o
endif
ifneq ($(PERSONALITY),zigbee)
OBJS += $(OUT)/sdk/thread-phy.o
endif
vpath %.c $(sort $(dir $(SRCS)))
vpath %.s79 $(sort $(dir $(ASMS)))
vpath %.s $(sort $(dir $(ASMS)))

$(OUT)/radio.bin: $(OUT)/radio.elf $(OUT)/radio.identity
	$(OBJCOPY) -O binary --gap-fill 0xff $< $(OUT)/app.bin
	head -c $(PREFIX) /dev/zero | cat - $(OUT)/app.bin > $@
	$(CROSS)size $<

# Makefile throughout: it holds the flags, VERSION among them.
$(OUT)/radio.elf: $(OBJS) $(OUT)/obj/radio-identity.o $(LIBFILES) $(OT_LIBFILES) $(LINK) Makefile
	$(CC) $(LDFLAGS) -o $@ $(OBJS) $(OUT)/obj/radio-identity.o \
	  -Wl,--start-group $(LIBFILES) $(OT_LIBFILES) -lc -lnosys -lgcc -Wl,--end-group

# Everything linked, hashed before the identity itself is linked in.
$(OUT)/radio-identity.c: $(OBJS) $(LIBFILES) $(OT_LIBFILES) $(LINK) Makefile
	identity=$$(cat $^ | sha256sum | cut -d ' ' -f1); \
	 printf 'const char rcp_identity[] = "%s";\n' "$$identity" > $@

$(OUT)/obj/radio-identity.o: $(OUT)/radio-identity.c
	$(CC) $(CFLAGS) -c -o $@ $<

$(OUT)/radio.identity: $(OUT)/radio-identity.c
	sed -n 's/.*"\([0-9a-f]*\)".*/\1/p' $< > $@

# The em260 main becomes the Zigbee personality's; the object is linked itself
# so the archive's copy, with its other definitions, is never pulled in.
$(OUT)/sdk/zigbee-main.o: $(LIB)/em260-spi-util-library-cortexm3-iar-$(PART)-em3xx/em260-spi-util-library.a Makefile | $(OUT)/obj
	mkdir -p $(dir $@)
	$(CROSS)ar p $< main.o > $@.tmp
	$(OBJCOPY) --redefine-sym main=zigbeeMain $@.tmp $@
	rm $@.tmp

# Only the PHY's early-ACK lookups go through Thread's source-match adapters;
# the stack's own child table is untouched.
$(OUT)/sdk/thread-phy.o: $(LIB)/zigbee-pro-stack-cortexm3-iar-$(PART)-em3xx/zigbee-pro-stack.a Makefile | $(OUT)/obj
	mkdir -p $(dir $@)
	$(CROSS)ar p $< phy.o > $@.tmp
	$(OBJCOPY) --redefine-sym emberChildIndex=threadRadioChildIndex \
	 --redefine-sym emLongIdDataPendingForDevice=threadRadioLongIdPending $@.tmp $@
	rm $@.tmp

$(OUT)/obj/%.c.o: %.c $(OUT)/sdk/$(PATCHED) $(BOARD) ncp-config.h Makefile | $(OUT)/obj
	$(CC) $(CFLAGS) -c -o $@ $<
$(OUT)/obj/%.s79.o: %.s79 Makefile | $(OUT)/obj
	$(CC) $(CFLAGS) -x assembler-with-cpp -c -o $@ $<
$(OUT)/obj/%.s.o: %.s Makefile | $(OUT)/obj
	$(CC) $(CFLAGS) -x assembler-with-cpp -c -o $@ $<

$(OUT)/sdk/$(PATCHED): $(HAL)/micro/cortexm3/ezsp-spi-serial-controller-autoselect.h $(SPI_PATCH)
	mkdir -p $(dir $@)
	cp $< $@
	$(if $(SPI_PATCH),patch -s $@ $(SPI_PATCH))

$(OUT)/dispatch.c: gen-dispatch.py $(ZB)/app/util/ezsp/command-functions.h $(ZB)/app/ncp/plugin/plugin.info | $(OUT)/obj
	python3 gen-dispatch.py $(ZB) $(foreach l,$(LIBS),$(LIB)/$(l)-cortexm3-iar-$(PART)-em3xx/$(l).a) > $@

$(OUT)/obj:
	mkdir -p $@

clean:
	rm -rf $(OUT)

.PHONY: clean
# A failed link leaves no image behind to pass for a built one.
.DELETE_ON_ERROR:
