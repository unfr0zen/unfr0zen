// The gen2 Nest's radio board: an EM357 talking EZSP-SPI to the OMAP on SC2,
// the SDK's own SC2 pins.
//
// The pin configuration is the one Nest's own radio firmware (its
// ip-modem-app.bin, at 0x08022188) sets, register for register:
//
//   PA0-PA3   SC2 SPI slave: MOSI, MISO, SCLK, nSSEL (pulled up)
//   PA4, PA5  packet trace (PTI_EN, PTI_DATA)
//   PA6, PA7  outputs, held as Nest's image holds them
//   PB2       nHOST_INT to the host
//   PB3       nSSEL_INT (EM260 compatibility, unused)
//   PB6       nWAKE (IRQB, pulled up; the host does not drive it)
//   PC2, PC4  SWO, SWDIO
//   PC5, PC6  TX_ACTIVE and its complement, for the front end
//
// Powered down the pins take Nest's power-down configuration with their
// output levels unchanged. The stack is given no radio-power pins to toggle
// (the mask is empty): the router never puts the radio to sleep.

#ifndef __BOARD_H__
#define __BOARD_H__

#include "hal/micro/cortexm3/spi-protocol.h"

#define ENABLE_ALT_FUNCTION_TX_ACTIVE
#define ENABLE_ALT_FUNCTION_NTX_ACTIVE

extern uint16_t gpioCfgPowerUp[6];
extern uint16_t gpioCfgPowerDown[6];
extern uint8_t gpioOutPowerUp[3];
extern uint8_t gpioOutPowerDown[3];
extern GpioMaskType gpioRadioPowerBoardMask;

// The pins EZSP's GPIO commands must not touch: the host link.
#define EZSP_PIN_BLACKOUT_MASK ((((GpioMaskType)1) << PORTA_PIN(0))   /* MOSI */      \
                                | (((GpioMaskType)1) << PORTA_PIN(1)) /* MISO */      \
                                | (((GpioMaskType)1) << PORTA_PIN(2)) /* SCLK */      \
                                | (((GpioMaskType)1) << PORTA_PIN(3)) /* nSSEL */     \
                                | (((GpioMaskType)1) << PORTB_PIN(2)) /* nHOST_INT */ \
                                | (((GpioMaskType)1) << PORTB_PIN(3)) /* nSSEL_INT */ \
                                | (((GpioMaskType)1) << PORTB_PIN(6))) /* nWAKE */

#define DEFINE_GPIO_RADIO_POWER_BOARD_MASK_VARIABLE() \
  GpioMaskType gpioRadioPowerBoardMask = 0

// Nest's words, PACFGL to PCCFGH: four bits a pin, pin 0 lowest.
#define DEFINE_POWERUP_GPIO_CFG_VARIABLES() \
  uint16_t gpioCfgPowerUp[6] = { 0x8494, 0x1199, 0x4141, 0x8811, 0x4919, 0x4994 }

#define DEFINE_POWERUP_GPIO_OUTPUT_DATA_VARIABLES() \
  uint8_t gpioOutPowerUp[3] = { 0xEF, 0x5E, 0x05 }

#define DEFINE_POWERDOWN_GPIO_CFG_VARIABLES() \
  uint16_t gpioCfgPowerDown[6] = { 0x8888, 0x1188, 0x8881, 0x8818, 0x8118, 0x4118 }

#define DEFINE_POWERDOWN_GPIO_OUTPUT_DATA_VARIABLES() \
  uint8_t gpioOutPowerDown[3] = { 0xEF, 0x5E, 0x05 }

#define SET_POWERUP_GPIO_CFG_REGISTERS() \
  GPIO_PACFGL = gpioCfgPowerUp[0];       \
  GPIO_PACFGH = gpioCfgPowerUp[1];       \
  GPIO_PBCFGL = gpioCfgPowerUp[2];       \
  GPIO_PBCFGH = gpioCfgPowerUp[3];       \
  GPIO_PCCFGL = gpioCfgPowerUp[4];       \
  GPIO_PCCFGH = gpioCfgPowerUp[5];

#define SET_POWERUP_GPIO_OUTPUT_DATA_REGISTERS() \
  GPIO_PAOUT = gpioOutPowerUp[0];                \
  GPIO_PBOUT = gpioOutPowerUp[1];                \
  GPIO_PCOUT = gpioOutPowerUp[2];

#define SET_POWERDOWN_GPIO_CFG_REGISTERS() \
  GPIO_PACFGL = gpioCfgPowerDown[0];       \
  GPIO_PACFGH = gpioCfgPowerDown[1];       \
  GPIO_PBCFGL = gpioCfgPowerDown[2];       \
  GPIO_PBCFGH = gpioCfgPowerDown[3];       \
  GPIO_PCCFGL = gpioCfgPowerDown[4];       \
  GPIO_PCCFGH = gpioCfgPowerDown[5];

#define SET_POWERDOWN_GPIO_OUTPUT_DATA_REGISTERS() \
  GPIO_PAOUT = gpioOutPowerDown[0];                \
  GPIO_PBOUT = gpioOutPowerDown[1];                \
  GPIO_PCOUT = gpioOutPowerDown[2];

// nWAKE wakes the chip from deep sleep.
#define WAKE_ON_PA0 false
#define WAKE_ON_PA1 false
#define WAKE_ON_PA2 false
#define WAKE_ON_PA3 false
#define WAKE_ON_PA4 false
#define WAKE_ON_PA5 false
#define WAKE_ON_PA6 false
#define WAKE_ON_PA7 false
#define WAKE_ON_PB0 false
#define WAKE_ON_PB1 false
#define WAKE_ON_PB2 false
#define WAKE_ON_PB3 false
#define WAKE_ON_PB4 false
#define WAKE_ON_PB5 false
#define WAKE_ON_PB6 true
#define WAKE_ON_PB7 false
#define WAKE_ON_PC0 false
#define WAKE_ON_PC1 false
#define WAKE_ON_PC2 false
#define WAKE_ON_PC3 false
#define WAKE_ON_PC4 false
#define WAKE_ON_PC5 false
#define WAKE_ON_PC6 false
#define WAKE_ON_PC7 false

#define halInternalInitBoard()     \
  do {                             \
    halInternalPowerUpBoard();     \
    halHostSerialInit();           \
  } while (0)

#define halInternalPowerDownBoard()                \
  do {                                             \
    halHostSerialPowerdown();                      \
    SET_POWERDOWN_GPIO_OUTPUT_DATA_REGISTERS()     \
    SET_POWERDOWN_GPIO_CFG_REGISTERS()             \
  } while (0)

#define halInternalPowerUpBoard()                  \
  do {                                             \
    SET_POWERUP_GPIO_OUTPUT_DATA_REGISTERS()       \
    SET_POWERUP_GPIO_CFG_REGISTERS()               \
    halHostSerialPowerup();                        \
    halInternalInitAdc();                          \
  } while (0)

#define halInternalSuspendBoard() do {} while (0)
#define halInternalResumeBoard()  do {} while (0)

#endif // __BOARD_H__
