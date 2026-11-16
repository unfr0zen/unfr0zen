/* Raw NAND reader and page programmer for the Nest gen 2 (j49), which the
 * app's j49 flow loads through x-loader's USB loader (at 0x80100000, with
 * GPMC CS0 and EP1 as x-loader left them): READ of raw pages, PROGRAM for
 * one page, ERASE for one block, UNLOCK for a whole-NAND restore, and a
 * reset.
 *
 * Programming is refused outside the boot0/root0 and boot1/root1
 * partitions (NAND 4-58 MiB and 58-112 MiB) and the two U-Boot
 * environment blocks (0x340000, 0x3a0000); erasing outside those two
 * blocks and the upper halves of boot0 and boot1 (8-12 MiB, 62-66 MiB),
 * where stock keeps nothing (its kernel is the 2.2 MB at their start) and
 * unfr0zen-gen2.py puts a kernel of its own. The boot loaders, the stock
 * kernels and the unit's configuration cannot be written by any request,
 * and no root slot can be erased, until UNLOCK (with its magic word) lifts
 * those limits for the rest of the session: a whole-NAND restore sends it,
 * and a setup that erases the blocks of the nestlocal it replaces. A page is only programmed if it is still erased
 * (data and ECC bytes all 0xff), so nothing already written can be changed. The GPMC write-protect output is held low
 * (protected) except while a page is programmed or a block erased.
 *
 * Protocol (little-endian 32-bit words, EP1 bulk):
 *   device -> 'USBh' GPMC_CONFIG1_0 GPMC_CONFIG7_0 id[8] CONTROL_IDCODE
 *                                                          once, on entry
 *   host   -> 'USBr' first_page page_count                 as the dumper
 *   device -> page_count * (2048 data + 64 OOB) raw bytes
 *   host   -> 'USBp' page 0, then 2112 raw bytes (data + OOB, ECC included)
 *   device -> 'USBs' page status  0 programmed, 1 refused (outside the
 *             ranges above), 2 the NAND reported a program failure,
 *             3 the NAND stayed busy, 4 the page is not erased
 *   host   -> 'USBE' block 0                               erase one block
 *   device -> 'USBs' block status  as for PROGRAM
 *   host   -> 'USBU' 'ALL!' 0                              lift the limits
 *   device -> 'USBs' 0 status      0 unlocked, 1 refused (wrong magic word)
 *   host   -> 'USBx' 0 0                                   reset the SoC
 *   host   -> 'USBb' machine length, then length bytes: the entry of a
 *             kernel x-loader loaded with the flasher, then its ATAGs
 *   device -> 'USBs' 0 status      0 booting, 1 refused (too long, or not
 *             whole words); then, booting, the flasher leaves USB and
 *             enters the kernel as a boot loader enters one: r0 0, r1 the
 *             machine number, r2 the ATAGs (at 0x80000100)
 *   device -> 'USBe' page                                  a read stayed busy; halts
 */
typedef unsigned char u8;
typedef unsigned short u16;
typedef unsigned int u32;

#define REG8(a)  (*(volatile u8 *)(a))
#define REG16(a) (*(volatile u16 *)(a))
#define REG32(a) (*(volatile u32 *)(a))

#define GPMC_CONFIG            0x6E000050
#define GPMC_WRITEPROTECT_HIGH (1 << 4)
#define GPMC_CONFIG1_0         0x6E000060
#define GPMC_CONFIG7_0         0x6E000078
#define GPMC_NAND_COMMAND_0    0x6E00007C
#define GPMC_NAND_ADDRESS_0    0x6E000080
#define GPMC_NAND_DATA_0       0x6E000084

#define CONTROL_IDCODE    0x4830A204 /* silicon version in bits 31-28 */
#define PRM_RSTCTRL       0x48307250
#define PRM_RSTCTRL_RESET 0x04

#define USB_BASE      0x480AB000
#define USB_POWER     (USB_BASE + 0x01)
#define USB_SOFTCONN  (1 << 6)
#define USB_INTRTXE   (USB_BASE + 0x06)
#define USB_INTRRXE   (USB_BASE + 0x08)
#define USB_INTRUSBE  (USB_BASE + 0x0B)
#define OTG_SYSCONFIG (USB_BASE + 0x404)
#define OTG_SOFTRESET (1 << 1)
#define OTG_SYSSTATUS (USB_BASE + 0x408)
#define OTG_RESETDONE (1 << 0)
#define EP1_FIFO      (USB_BASE + 0x24)
#define EP1_TXCSR     (USB_BASE + 0x112)
#define EP1_RXCSR     (USB_BASE + 0x116)
#define EP1_RXCOUNT   (USB_BASE + 0x118)
#define PKTRDY        0x0001
#define PACKET        512

#define PAGE_RAW        (2048 + 64)
#define PAGES_PER_MIB   512
#define PAGES_PER_BLOCK 64
#define SLOTS_FIRST     (4 * PAGES_PER_MIB) /* boot0, root0, boot1, root1 */
#define SLOTS_END       (112 * PAGES_PER_MIB)
#define ENV0_BLOCK      (0x340000 / 2048 / PAGES_PER_BLOCK)
#define ENV1_BLOCK      (0x3a0000 / 2048 / PAGES_PER_BLOCK)
#define BLOCKS_PER_MIB  (PAGES_PER_MIB / PAGES_PER_BLOCK)
#define BOOT0_TAIL      (8 * BLOCKS_PER_MIB) /* 4 MiB each */
#define BOOT1_TAIL      (62 * BLOCKS_PER_MIB)
#define TAIL_BLOCKS     (4 * BLOCKS_PER_MIB)

#define PACK4(a, b, c, d) (((d) << 24) | ((c) << 16) | ((b) << 8) | (a))
#define CMD_HELLO         PACK4('U', 'S', 'B', 'h')
#define CMD_READ          PACK4('U', 'S', 'B', 'r')
#define CMD_PROGRAM       PACK4('U', 'S', 'B', 'p')
#define CMD_STATUS        PACK4('U', 'S', 'B', 's')
#define CMD_RESET         PACK4('U', 'S', 'B', 'x')
#define CMD_ERASE         PACK4('U', 'S', 'B', 'E')
#define CMD_ERROR         PACK4('U', 'S', 'B', 'e')
#define CMD_UNLOCK        PACK4('U', 'S', 'B', 'U')
#define CMD_BOOT          PACK4('U', 'S', 'B', 'b')
#define ATAGS             0x80000100
#define ATAGS_LIMIT       1024
#define UNLOCK_MAGIC      PACK4('A', 'L', 'L', '!')

#define STATUS_OK      0
#define STATUS_REFUSED 1
#define STATUS_FAILED  2
#define STATUS_BUSY    3
#define STATUS_WRITTEN 4

#define ECC_OFFSET 36 /* OOB: BCH codes from here to the end */

void enter_kernel(u32 entry, u32 machine, u32 atags) __attribute__((noreturn));

/* x-loader leaves the caches off, so every instruction would be fetched
 * from SDRAM: the I-cache and branch prediction go on first (the MMU stays
 * off, and the code never changes). */
__asm__(".text\n"
        ".global _start\n"
        "_start:\n"
        "	ldr sp, =0x80800000\n"
        "	mov r0, #0\n"
        "	mcr p15, 0, r0, c7, c5, 0\n" /* invalidate the I-cache */
        "	mcr p15, 0, r0, c7, c5, 6\n" /* and the branch predictor */
        "	mrc p15, 0, r0, c1, c0, 0\n"
        "	orr r0, r0, #0x1800\n" /* SCTLR.I, SCTLR.Z */
        "	mcr p15, 0, r0, c1, c0, 0\n"
        "	isb\n"
        "	bl flasher_main\n"
        "1:	b 1b\n"
        /* enter_kernel(entry, machine, atags): the CPU as U-Boot leaves it for
         * Linux (cleanup_before_linux), which x-loader does not: L2 invalidated
         * by the ROM (a GP device's service 1, as U-Boot's
         * omap3_gp_romcode_call; reset leaves it on and holding garbage, which
         * the kernel's cache cleaning would write to SDRAM), the MMU, caches,
         * branch prediction and alignment faults off (x-loader turns those on,
         * and the kernel's decompressor reads unaligned), the I-cache
         * invalidated. The ROM call may change r0-r5 and r12. */
        "	.global enter_kernel\n"
        "enter_kernel:\n"
        "	push {r0, r1, r2}\n"
        "	mov r12, #1\n"
        "	mov r0, #0\n"
        "	mcr p15, 0, r0, c7, c10, 4\n" /* DSB */
        "	mcr p15, 0, r0, c7, c10, 5\n" /* DMB */
        "	.word 0xe1600070\n"           /* SMC #0 */
        "	ldmfd sp!, {r4, r5, r6}\n"
        "	mrc p15, 0, r0, c1, c0, 0\n"
        "	bic r0, r0, #0x1800\n" /* SCTLR.I, SCTLR.Z */
        "	bic r0, r0, #0x0007\n" /* SCTLR.C, SCTLR.A, SCTLR.M */
        "	mcr p15, 0, r0, c1, c0, 0\n"
        "	isb\n"
        "	mov r0, #0\n"
        "	mcr p15, 0, r0, c7, c5, 0\n" /* invalidate the I-cache */
        "	mcr p15, 0, r0, c7, c5, 6\n" /* and the branch predictor */
        "	dsb\n"
        "	isb\n"
        "	mov r1, r5\n"
        "	mov r2, r6\n"
        "	bx r4\n"
        ".ltorg\n");

struct stream {
	u32 fill;
};

static void delay(u32 loops) { __asm__ volatile("1: subs %0, %0, #1\n bne 1b" : "=r"(loops) : "0"(loops)); }

static void put8(struct stream *s, u8 byte)
{
	if (s->fill == 0)
		while (REG16(EP1_TXCSR) & PKTRDY)
			;
	REG8(EP1_FIFO) = byte;
	if (++s->fill == PACKET) {
		REG16(EP1_TXCSR) |= PKTRDY;
		s->fill = 0;
	}
}

static void put32(struct stream *s, u32 word)
{
	put8(s, word);
	put8(s, word >> 8);
	put8(s, word >> 16);
	put8(s, word >> 24);
}

static void flush(struct stream *s)
{
	if (s->fill) {
		REG16(EP1_TXCSR) |= PKTRDY;
		s->fill = 0;
	}
}

/* One USB packet; bytes beyond `max` are drained and dropped. A packet
 * that fits a word-aligned buffer in whole words is read a word at a time. */
static u32 recv(u8 *buffer, u32 max)
{
	u32 count, i;
	u8 byte;

	while (!(REG16(EP1_RXCSR) & PKTRDY))
		;
	count = REG16(EP1_RXCOUNT);
	if (count <= max && !(count & 3) && !((u32)buffer & 3)) {
		for (i = 0; i < count; i += 4)
			*(u32 *)(buffer + i) = REG32(EP1_FIFO);
	} else {
		for (i = 0; i < count; i++) {
			byte = REG8(EP1_FIFO);
			if (i < max)
				buffer[i] = byte;
		}
	}
	REG16(EP1_RXCSR) &= ~PKTRDY;
	return count;
}

/* One word into the EP1 FIFO, `fill` bytes into the packet; the new fill. */
static inline __attribute__((always_inline)) u32 fifo_word(u32 fill, u32 word)
{
	if (fill == 0)
		while (REG16(EP1_TXCSR) & PKTRDY)
			;
	REG32(EP1_FIFO) = word;
	fill += 4;
	if (fill == PACKET) {
		REG16(EP1_TXCSR) |= PKTRDY;
		fill = 0;
	}
	return fill;
}

static void nand_command(int wide, u8 command)
{
	if (wide)
		REG16(GPMC_NAND_COMMAND_0) = command;
	else
		REG8(GPMC_NAND_COMMAND_0) = command;
}

static void nand_address(int wide, u8 address)
{
	if (wide)
		REG16(GPMC_NAND_ADDRESS_0) = address;
	else
		REG8(GPMC_NAND_ADDRESS_0) = address;
}

static u16 nand_data(int wide) { return wide ? REG16(GPMC_NAND_DATA_0) : REG8(GPMC_NAND_DATA_0); }

/* Poll READ STATUS until ready; the status byte, or -1 if it never was. */
static int nand_status(int wide)
{
	u32 i;
	u16 status;

	delay(1000); /* tWB: give the chip time to drop ready */
	nand_command(wide, 0x70);
	for (i = 0; i < 10000000; i++) {
		status = nand_data(wide);
		if (status & 0x40)
			return status & 0xff;
	}
	return -1;
}

/* Column 0, three row cycles (256/512 MiB parts). */
static void nand_page_address(int wide, u32 page)
{
	nand_address(wide, 0);
	nand_address(wide, 0);
	nand_address(wide, page);
	nand_address(wide, page >> 8);
	nand_address(wide, page >> 16);
}

/* READ: `count` raw pages from `first`, streamed a FIFO word at a time
 * (each page is whole words, so a word never spans two). The page whose
 * read stayed busy, after what was read before it, or -1 when all were. */
static int nand_read_pages(int wide, u32 first, u32 count)
{
	u32 page, i, fill = 0, lo, hi;

	for (page = first; page < first + count; page++) {
		nand_command(wide, 0x00);
		nand_page_address(wide, page);
		nand_command(wide, 0x30);
		if (nand_status(wide) < 0) {
			if (fill)
				REG16(EP1_TXCSR) |= PKTRDY;
			return page;
		}
		nand_command(wide, 0x00); /* READ MODE: data out again after status */
		for (i = 0; i < PAGE_RAW / 4; i++) {
			if (wide) {
				lo = REG16(GPMC_NAND_DATA_0);
				hi = REG16(GPMC_NAND_DATA_0);
				fill = fifo_word(fill, lo | hi << 16);
			} else {
				lo = REG8(GPMC_NAND_DATA_0);
				lo |= REG8(GPMC_NAND_DATA_0) << 8;
				hi = REG8(GPMC_NAND_DATA_0);
				hi |= REG8(GPMC_NAND_DATA_0) << 8;
				fill = fifo_word(fill, lo | hi << 16);
			}
		}
	}
	if (fill)
		REG16(EP1_TXCSR) |= PKTRDY;
	return -1;
}

/* `unlocked` (by UNLOCK): every block may be erased and every page
 * programmed. It is flasher_main's: nothing zeroes .bss on the unit. */
static int erasable(int unlocked, u32 block)
{
	return unlocked || block == ENV0_BLOCK || block == ENV1_BLOCK ||
	       (block >= BOOT0_TAIL && block < BOOT0_TAIL + TAIL_BLOCKS) ||
	       (block >= BOOT1_TAIL && block < BOOT1_TAIL + TAIL_BLOCKS);
}

static int programmable(int unlocked, u32 page)
{
	return (page >= SLOTS_FIRST && page < SLOTS_END) || erasable(unlocked, page / PAGES_PER_BLOCK);
}

/* Every data and ECC bit still 1: not programmed since its erase. */
static int nand_page_erased(int wide, u32 page)
{
	u32 i;
	u16 word;

	nand_command(wide, 0x00);
	nand_page_address(wide, page);
	nand_command(wide, 0x30);
	if (nand_status(wide) < 0)
		return -1;
	nand_command(wide, 0x00);
	for (i = 0; i < PAGE_RAW; i += wide ? 2 : 1) {
		word = wide ? REG16(GPMC_NAND_DATA_0) : REG8(GPMC_NAND_DATA_0) | 0xff00;
		if ((i < 2048 || i >= 2048 + ECC_OFFSET) && word != 0xffff)
			return 0;
	}
	return 1;
}

static u32 nand_program_page(int wide, int unlocked, u32 page, const u32 *data)
{
	u32 i;
	int status;

	if (!programmable(unlocked, page))
		return STATUS_REFUSED;
	status = nand_page_erased(wide, page);
	if (status < 0)
		return STATUS_BUSY;
	if (!status)
		return STATUS_WRITTEN;
	REG32(GPMC_CONFIG) |= GPMC_WRITEPROTECT_HIGH;
	nand_command(wide, 0x80);
	nand_page_address(wide, page);
	/* The page's bytes in order: each word is four, low byte first. */
	for (i = 0; i < PAGE_RAW / 4; i++) {
		if (wide) {
			REG16(GPMC_NAND_DATA_0) = data[i];
			REG16(GPMC_NAND_DATA_0) = data[i] >> 16;
		} else {
			REG8(GPMC_NAND_DATA_0) = data[i];
			REG8(GPMC_NAND_DATA_0) = data[i] >> 8;
			REG8(GPMC_NAND_DATA_0) = data[i] >> 16;
			REG8(GPMC_NAND_DATA_0) = data[i] >> 24;
		}
	}
	nand_command(wide, 0x10);
	status = nand_status(wide);
	REG32(GPMC_CONFIG) &= ~GPMC_WRITEPROTECT_HIGH;
	nand_command(wide, 0x00);
	if (status < 0)
		return STATUS_BUSY;
	return (status & 0x01) ? STATUS_FAILED : STATUS_OK;
}

static u32 nand_erase_block(int wide, int unlocked, u32 block)
{
	u32 page = block * PAGES_PER_BLOCK;
	int status;

	if (!erasable(unlocked, block))
		return STATUS_REFUSED;
	REG32(GPMC_CONFIG) |= GPMC_WRITEPROTECT_HIGH;
	nand_command(wide, 0x60);
	nand_address(wide, page);
	nand_address(wide, page >> 8);
	nand_address(wide, page >> 16);
	nand_command(wide, 0xd0);
	status = nand_status(wide);
	REG32(GPMC_CONFIG) &= ~GPMC_WRITEPROTECT_HIGH;
	nand_command(wide, 0x00);
	if (status < 0)
		return STATUS_BUSY;
	return (status & 0x01) ? STATUS_FAILED : STATUS_OK;
}

void flasher_main(void)
{
	struct stream s = {0};
	u32 config1 = REG32(GPMC_CONFIG1_0);
	u32 config7 = REG32(GPMC_CONFIG7_0);
	/* DEVICETYPE NAND, chip select valid; DEVICESIZE 1 means 16-bit. */
	int usable = ((config1 >> 10) & 3) == 2 && (config7 & (1 << 6));
	int wide = ((config1 >> 12) & 3) == 1;
	int unlocked = 0;
	u32 command[3];
	u32 page_data[PAGE_RAW / 4]; /* word-aligned for recv and programming */
	u32 got, status;
	int i, busy;

	REG32(GPMC_CONFIG) &= ~GPMC_WRITEPROTECT_HIGH;

	put32(&s, CMD_HELLO);
	put32(&s, config1);
	put32(&s, config7);
	if (usable) {
		nand_command(wide, 0x90);
		nand_address(wide, 0x00);
	}
	for (i = 0; i < 8; i++)
		put8(&s, usable ? nand_data(wide) : 0);
	put32(&s, REG32(CONTROL_IDCODE));
	flush(&s);
	if (!usable)
		for (;;)
			;

	for (;;) {
		if (recv((u8 *)command, sizeof(command)) != sizeof(command))
			continue;
		if (command[0] == CMD_READ) {
			busy = nand_read_pages(wide, command[1], command[2]);
			if (busy >= 0) {
				put32(&s, CMD_ERROR);
				put32(&s, busy);
				flush(&s);
				for (;;)
					;
			}
		} else if (command[0] == CMD_PROGRAM) {
			for (got = 0; got < PAGE_RAW;)
				got += recv((u8 *)page_data + got, PAGE_RAW - got);
			status = nand_program_page(wide, unlocked, command[1], page_data);
			put32(&s, CMD_STATUS);
			put32(&s, command[1]);
			put32(&s, status);
			flush(&s);
		} else if (command[0] == CMD_ERASE) {
			status = nand_erase_block(wide, unlocked, command[1]);
			put32(&s, CMD_STATUS);
			put32(&s, command[1]);
			put32(&s, status);
			flush(&s);
		} else if (command[0] == CMD_UNLOCK) {
			unlocked = command[1] == UNLOCK_MAGIC;
			put32(&s, CMD_STATUS);
			put32(&s, 0);
			put32(&s, unlocked ? STATUS_OK : STATUS_REFUSED);
			flush(&s);
		} else if (command[0] == CMD_BOOT) {
			/* The entry word lands just below the ATAGs. */
			status = command[2] > 4 && command[2] <= 4 + ATAGS_LIMIT && command[2] % 4 == 0
			             ? STATUS_OK
			             : STATUS_REFUSED;
			if (status == STATUS_OK)
				for (got = 0; got < command[2];)
					got += recv((u8 *)(ATAGS - 4) + got, command[2] - got);
			put32(&s, CMD_STATUS);
			put32(&s, 0);
			put32(&s, status);
			flush(&s);
			if (status == STATUS_OK) {
				/* Off USB once the host has the status, as
				 * x-loader leaves it for a kernel
				 * (udc_disconnect), and the controller reset to
				 * how a boot from NAND finds it: the kernel's
				 * gadget connects afresh. */
				while (REG16(EP1_TXCSR) & PKTRDY)
					;
				REG16(USB_INTRTXE) = 0;
				REG16(USB_INTRRXE) = 0;
				REG8(USB_INTRUSBE) = 0;
				REG8(USB_POWER) &= ~USB_SOFTCONN;
				REG32(OTG_SYSCONFIG) |= OTG_SOFTRESET;
				while (!(REG32(OTG_SYSSTATUS) & OTG_RESETDONE))
					;
				enter_kernel(REG32(ATAGS - 4), command[1], ATAGS);
			}
		} else if (command[0] == CMD_RESET) {
			REG32(PRM_RSTCTRL) |= PRM_RSTCTRL_RESET;
			for (;;)
				;
		}
	}
}
