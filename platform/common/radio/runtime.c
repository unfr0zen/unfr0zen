#include PLATFORM_HEADER
#include "stack/include/ember.h"
#include "hal/hal.h"
#include "serial/serial.h"
#include "sim-eeprom/sim-eeprom.h"

// The SDK's GCC startup trusts whoever jumped to the reset vector to have
// loaded our stack pointer; Nest's own image loads it itself on entry, after
// Nest's bootloader. So do we: the Makefile wraps SystemInit, the startup's
// first call, with this.
__attribute__((naked)) void __wrap_SystemInit(void)
{
  __asm volatile ("ldr r0, =__StackTop\n"
                  "msr msp, r0\n"
                  "movs r0, #0\n"
                  "msr control, r0\n"
                  "isb\n"
                  "b __real_SystemInit\n");
}

// The SDK's default: erase pages as the Simulated EEPROM asks, repair it (and
// reset, for a failed write) when it goes wrong.
void halSimEepromCallback(EmberStatus status)
{
  static bool repairActive = false;
  switch (status) {
    case EMBER_SIM_EEPROM_ERASE_PAGE_GREEN:
      halSimEepromErasePage();
      return;
    case EMBER_SIM_EEPROM_ERASE_PAGE_RED:
    case EMBER_SIM_EEPROM_FULL:
      if (halSimEepromPagesRemainingToBeErased() > 0) {
        while (halSimEepromErasePage()) {
        }
        return;
      }
    // fall through: page rotation is stuck, repair
    case EMBER_ERR_FLASH_WRITE_INHIBITED:
    case EMBER_ERR_FLASH_VERIFY_FAILED:
      if (repairActive) {
        return;
      }
      repairActive = true;
      halInternalSimEeRepair(false);
      if (status == EMBER_ERR_FLASH_VERIFY_FAILED) {
        halInternalSysReset(RESET_FLASH_VERIFY);
      } else if (status == EMBER_ERR_FLASH_WRITE_INHIBITED) {
        halInternalSysReset(RESET_FLASH_INHIBIT);
      }
      repairActive = false;
      return;
    default:
      return;
  }
}

// The radio has no LEDs.
void halStackIndicateActivity(bool turnOn)
{
}

// No UART and no serial port: the host link is SPI.
void halInternalPowerUpUart(void)
{
}

void halInternalPowerDownUart(void)
{
}

EmberStatus emberSerialInit(uint8_t port, SerialBaudRate rate, SerialParity parity, uint8_t stopBits)
{
  return EMBER_SERIAL_INVALID_PORT;
}

EmberStatus emberSerialGuaranteedPrintf(uint8_t port, const char *formatString, ...)
{
  return EMBER_SERIAL_INVALID_PORT;
}

// Radio coexistence (PTA) is not wired on the Nest.
void halStackRadioHoldOffPowerUp(void)
{
}

void halStackRadioHoldOffPowerDown(void)
{
}

EmberStatus halPtaStackEvent(uint32_t event, uint32_t supplement)
{
  return EMBER_SUCCESS;
}

bool halPtaIsEnabled(void)
{
  return false;
}

EmberStatus halPtaSetBool(uint32_t option, bool value)
{
  return EMBER_ERR_FATAL;
}

EmberStatus halSetRadioHoldOff(bool enabled)
{
  return enabled ? EMBER_ERR_FATAL : EMBER_SUCCESS;
}

// The C runtime the IAR-built libraries and GCC's own code call, with no libc.
// (The Makefile keeps GCC from turning halCommonMem*'s loops back into these.)
void *memcpy(void *dest, const void *src, size_t n)
{
  halCommonMemMove(dest, src, n);
  return dest;
}

void *memmove(void *dest, const void *src, size_t n)
{
  halCommonMemMove(dest, src, n);
  return dest;
}

void *memset(void *dest, int c, size_t n)
{
  halCommonMemSet(dest, c, n);
  return dest;
}

void __aeabi_memcpy(void *dest, const void *src, size_t n) { memcpy(dest, src, n); }
void __aeabi_memcpy4(void *dest, const void *src, size_t n) { memcpy(dest, src, n); }
void __aeabi_memset(void *dest, size_t n, int c) { memset(dest, c, n); }
void __aeabi_memclr4(void *dest, size_t n) { memset(dest, 0, n); }

int abs(int x)
{
  return x < 0 ? -x : x;
}
