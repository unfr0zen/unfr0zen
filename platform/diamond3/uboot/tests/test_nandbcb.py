"""Exercise recovery boot-control inspection with corrupt NAND headers."""
from pathlib import Path
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]
SOURCE = Path("arch/arm/mach-imx/cmd_nandbcb.c")


def function(source, name):
    start = source.index(name + "(")
    start = source.rfind("\n", 0, start) + 1
    end = source.index("\n}", start) + 2
    return source[start:end]


class BootControlTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.directory = tempfile.TemporaryDirectory()
        cls.addClassCleanup(cls.directory.cleanup)
        work = Path(cls.directory.name)
        patched = work / "nandbcb.c"
        subprocess.run(["patch", "-s", "-o", str(patched),
                        str(ROOT / "upstream" / SOURCE),
                        str(ROOT / "patches/recovery/arch-arm-mach-imx-cmd_nandbcb.c.patch")],
                       check=True)
        source = patched.read_text()
        harness = r'''
#include <assert.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <errno.h>
typedef uint32_t u32;
typedef uint8_t u8;
typedef unsigned char u_char;
typedef long long test_loff_t;
#define loff_t test_loff_t
#include "imx-nandbcb.h"
#define GFP_KERNEL 0
#define FIRMWARE_EXTRA_ONE 512
#define kzalloc(size, flags) calloc(1, size)
#define kfree free
struct mtd_info { unsigned writesize; unsigned long long size; };
struct boot_config { struct mtd_info *mtd; unsigned search_area_size_in_bytes; };
static struct { unsigned misc_flags; } plat_config;
static int g_boot_search_count = 4;
static unsigned g_boot_search_stride = 0x20000;
static int scenario, data_reads, fcb_reads;
static int nand_read_skip_bad(struct mtd_info *mtd, loff_t off, size_t *size,
                             size_t *actual, unsigned long long limit, u8 *buf)
{
    (void)mtd; (void)off; (void)limit;
    memset(buf, 0, *size);
    if (actual) *actual = *size;
    if (*size == sizeof(struct dbbt_block)) {
        struct dbbt_block *dbbt = (void *)buf;
        dbbt->fingerprint = DBBT_FINGERPRINT;
        dbbt->version = DBBT_VERSION_1;
        if (scenario == 1) dbbt->dbbtpages = UINT32_MAX;
        if (scenario == 2 || scenario == 5) memset(buf, 0xff, *size);
        if (scenario == 3 || scenario == 4 || scenario == 8) dbbt->dbbtpages = 1;
    } else {
        data_reads++;
        ((u32 *)buf)[1] = scenario == 4 ? UINT32_MAX : 2;
        ((u32 *)buf)[2] = 7;
        ((u32 *)buf)[3] = 19;
    }
    return 0;
}
static int read_fcb(struct boot_config *cfg, struct fcb_block *fcb, loff_t off)
{
    (void)cfg;
    fcb_reads++;
    memset(fcb, 0, sizeof(*fcb));
    if (scenario == 6) return 1;
    if (scenario == 7 && !off) return 1;
    fcb->fingerprint = FCB_FINGERPRINT;
    fcb->fw1_start = 0x200;
    fcb->fw2_start = 0x500;
    return 0;
}
'''
        harness += "\n".join(function(source, name) for name in
                             ["read_dbbt", "dump_structure", "check_fingerprint", "nandbcb_dump"])
        harness += r'''
int main(int argc, char **argv)
{
    assert(argc == 2);
    scenario = atoi(argv[1]);
    struct mtd_info mtd = { .writesize = 2048, .size = 0x20000000 };
    struct boot_config cfg = { .mtd = &mtd, .search_area_size_in_bytes = 0x80000 };
    if (scenario < 5) {
        struct dbbt_block dbbt;
        u8 page[2048];
        int ret = read_dbbt(&cfg, &dbbt, page, 0x80000);
        if (scenario == 0 || scenario == 3) assert(ret == 0);
        else assert(ret == 1);
        if (scenario == 0 || scenario == 1 || scenario == 2) assert(data_reads == 0);
    } else {
        int ret = nandbcb_dump(&cfg);
        if (scenario == 6) assert(ret != 0);
        else assert(ret == 0);
        assert(fcb_reads == 4);
    }
    return 0;
}
'''
        test = work / "test.c"
        test.write_text(harness)
        cls.binary = work / "test"
        subprocess.run(["cc", "-std=gnu11", "-Wall", "-Wextra", "-Werror",
                        "-Wno-sign-compare", "-I", str(ROOT / "upstream/arch/arm/include/asm/mach-imx"),
                        str(test), "-o", str(cls.binary)], check=True)

    def run_case(self, case):
        return subprocess.run([str(self.binary), str(case)], check=True,
                              capture_output=True, text=True, timeout=2).stdout

    def test_empty_bad_block_table(self):
        self.run_case(0)

    def test_reject_unbounded_page_count(self):
        self.run_case(1)

    def test_reject_erased_header(self):
        self.run_case(2)

    def test_read_bad_block_entries(self):
        self.run_case(3)

    def test_reject_unbounded_entry_count(self):
        self.run_case(4)

    def test_valid_fcb_without_dbbt(self):
        self.assertIn("fingerprint = 0x20424346", self.run_case(5))

    def test_missing_fcb(self):
        self.assertNotIn("fw1_start =", self.run_case(6))

    def test_dump_bad_block_entries(self):
        self.assertIn("7 19 ", self.run_case(8))

    def test_skip_unreadable_first_fcb(self):
        self.assertIn("fingerprint = 0x20424346", self.run_case(7))


if __name__ == "__main__":
    unittest.main()
