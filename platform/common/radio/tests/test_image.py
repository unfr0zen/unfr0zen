"""The built radio images, laid out for Nest's zb-loader and bootloader, and
carrying the radio Makefile's VERSION where nestlocal reads it: the gen3's EM3581
image (build/radio/radio.bin, or $NESTLOCAL_RADIO) and the gen2's EM357 Zigbee
(build/radio-em357-zigbee/radio.bin, or $NESTLOCAL_RADIO_EM357) and Thread
images. Each skips when its image has not been built.

Run from the top: python3 -m unittest discover -s platform/common/radio/tests
"""
import os
import re
import struct
import unittest

TOP = os.path.join(os.path.dirname(__file__), "..", "..", "..", "..")

LOAD = 0x08002000  # where zb-loader --app-easyload writes the image
APP_ADDRESS_TABLE_TYPE = 0x0AA7
SIMEEPROM_SIZE = 8 * 1024


class Layout:
    image = None
    aat = None  # the application address table
    flash_end = None
    ram = None
    device = None  # crates/nestlocal/src/device/<device>.rs, which says where nestlocal reads the version

    @classmethod
    def setUpClass(cls):
        if not os.path.exists(cls.image):
            raise unittest.SkipTest(f"no radio image at {cls.image}")
        with open(cls.image, "rb") as f:
            cls.data = f.read()
        cls.end = LOAD + len(cls.data)

    def word(self, address):
        return struct.unpack_from("<I", self.data, address - LOAD)[0]

    def test_the_bootloaders_pages_are_left_blank(self):
        self.assertEqual(self.data[:self.aat - LOAD], bytes(self.aat - LOAD))

    def test_the_address_table_starts_the_application(self):
        self.assertEqual(struct.unpack_from("<H", self.data, self.aat + 0x10 - LOAD)[0], APP_ADDRESS_TABLE_TYPE)
        stack, reset = self.word(self.aat), self.word(self.aat + 4)
        self.assertTrue(self.ram[0] < stack <= self.ram[1] and stack % 8 == 0, hex(stack))
        self.assertEqual(reset & 1, 1, "the reset vector is a Thumb address")
        self.assertTrue(self.aat < reset & ~1 < self.end, hex(reset))
        vectors = self.word(self.aat + 0x14)
        self.assertTrue(self.aat <= vectors < self.end, hex(vectors))

    def test_the_image_stops_short_of_the_simulated_eeprom(self):
        simee = self.word(self.aat + 0x54)
        self.assertEqual(simee, self.flash_end - SIMEEPROM_SIZE)
        self.assertLessEqual(self.end, simee)

    def test_nestlocal_reads_the_makefiles_version(self):
        with open(os.path.join(TOP, "platform", "common", "radio", "Makefile")) as f:
            version = int(re.search(r"^VERSION = (\d+)$", f.read(), re.M).group(1))
        with open(os.path.join(TOP, "crates", "nestlocal", "src", "device", f"{self.device}.rs")) as f:
            at = int(re.search(r"version_at: (0x[0-9A-Fa-f]+),", f.read()).group(1), 16)
        # customerApplicationVersion, in the address table.
        self.assertEqual(at, self.aat + 0x58 - LOAD)
        self.assertEqual(struct.unpack_from("<H", self.data, at)[0], version)


class Em3581(Layout, unittest.TestCase):
    image = os.environ.get("NESTLOCAL_RADIO") or os.path.join(TOP, "build", "radio", "radio.bin")
    aat = 0x08004000  # past Nest's 16 KiB bootloader
    flash_end = 0x08040000
    ram = (0x20000000, 0x20008000)
    device = "diamond3"


class Em357Zigbee(Layout, unittest.TestCase):
    image = os.environ.get("NESTLOCAL_RADIO_EM357") or os.path.join(TOP, "build", "radio-em357-zigbee", "radio.bin")
    aat = LOAD
    flash_end = 0x08030000
    ram = (0x20000000, 0x20003000)
    device = "j49"


class Em357Thread(Em357Zigbee):
    image = os.path.join(TOP, "build", "radio-em357-thread", "radio.bin")


if __name__ == "__main__":
    unittest.main()
