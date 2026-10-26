// The gen3 Nest's radio board: an EM3581 talking EZSP-SPI to the i.MX6 on SC1.
//
// The pin configuration is the one Nest's own radio firmware (ip-modem-app.bin)
// sets, register for register:
//
//   PB1-PB4   SC1 SPI slave: MISO, MOSI, SCLK, nSSEL (pulled up)
//   PB6       nWAKE from the host (IRQB, pulled up)
//   PB7       nHOST_INT to the host
//   PA4, PA5  packet trace (PTI_EN, PTI_DATA)
//   PA7       REG_EN (the external regulator enable)
//   PC5       TX_ACTIVE, while the radio is powered
//   PC2, PC4  SWO, SWDIO
//
// With the radio powered down PB0 is pulled up and PC5 driven low, the
// SDK's dev0680 front-end mask (PB0, PB5, PC5), as Nest's image keeps it.

#ifndef __BOARD_H__
#define __BOARD_H__

#include "hal/micro/cortexm3/spi-protocol.h"

#define ENABLE_ALT_FUNCTION_REG_EN
#define ENABLE_ALT_FUNCTION_TX_ACTIVE

extern uint16_t gpioCfgPowerUp[6];
extern uint16_t gpioCfgPowerDown[6];
extern uint8_t gpioOutPowerUp[3];
extern uint8_t gpioOutPowerDown[3];
extern GpioMaskType gpioRadioPowerBoardMask;

// The pins EZSP's GPIO commands must not touch: the host link.
#define EZSP_PIN_BLACKOUT_MASK ((((GpioMaskType)1) << PORTB_PIN(1))   /* MISO */      \
                                | (((GpioMaskType)1) << PORTB_PIN(2)) /* MOSI */      \
                                | (((GpioMaskType)1) << PORTB_PIN(3)) /* SCLK */      \
                                | (((GpioMaskType)1) << PORTB_PIN(4)) /* nSSEL */     \
                                | (((GpioMaskType)1) << PORTB_PIN(6)) /* nWAKE */     \
                                | (((GpioMaskType)1) << PORTB_PIN(7))) /* nHOST_INT */

#define DEFINE_GPIO_RADIO_POWER_BOARD_MASK_VARIABLE()                   \
  GpioMaskType gpioRadioPowerBoardMask = { BIT32(PORTB_PIN(0))          \
                                           | BIT32(PORTB_PIN(5))        \
                                           | BIT32(PORTC_PIN(5)) }

#define DEFINE_POWERUP_GPIO_CFG_VARIABLES()                                       \
  uint16_t gpioCfgPowerUp[6] = {                                                  \
    ((GPIOCFG_OUT       << PA0_CFG_BIT)                                           \
     | (GPIOCFG_OUT_ALT << PA1_CFG_BIT)                                           \
     | (GPIOCFG_IN      << PA2_CFG_BIT)                                           \
     | (GPIOCFG_OUT     << PA3_CFG_BIT)),                                         \
    ((GPIOCFG_OUT_ALT   << PA4_CFG_BIT)                       /* PTI_EN */        \
     | (GPIOCFG_OUT_ALT << PA5_CFG_BIT)                       /* PTI_DATA */      \
     | (GPIOCFG_OUT     << PA6_CFG_BIT)                                           \
     | (GPIOCFG_OUT     << PA7_CFG_BIT)),                     /* REG_EN */        \
    ((GPIOCFG_IN        << PB0_CFG_BIT)                                           \
     | (GPIOCFG_OUT_ALT << PB1_CFG_BIT)                       /* MISO */          \
     | (GPIOCFG_IN      << PB2_CFG_BIT)                       /* MOSI */          \
     | (GPIOCFG_IN      << PB3_CFG_BIT)),                     /* SCLK */          \
    ((GPIOCFG_IN_PUD    << PB4_CFG_BIT)                       /* nSSEL */         \
     | (GPIOCFG_IN      << PB5_CFG_BIT)                                           \
     | (GPIOCFG_IN_PUD  << PB6_CFG_BIT)                       /* nWAKE */         \
     | (GPIOCFG_OUT     << PB7_CFG_BIT)),                     /* nHOST_INT */     \
    ((GPIOCFG_OUT_ALT   << PC0_CFG_BIT)                                           \
     | (GPIOCFG_OUT     << PC1_CFG_BIT)                                           \
     | (GPIOCFG_OUT_ALT << PC2_CFG_BIT)                       /* SWO */           \
     | (GPIOCFG_IN      << PC3_CFG_BIT)),                                         \
    ((GPIOCFG_IN        << PC4_CFG_BIT)                       /* SWDIO */         \
     | (GPIOCFG_OUT_ALT << PC5_CFG_BIT)                       /* TX_ACTIVE */     \
     | (GPIOCFG_OUT     << PC6_CFG_BIT)                                           \
     | (GPIOCFG_IN      << PC7_CFG_BIT))                                          \
  }

#define DEFINE_POWERUP_GPIO_OUTPUT_DATA_VARIABLES() \
  uint8_t gpioOutPowerUp[3] = { 0xE3, 0xFF, 0x05 }

// Powered down only the radio's pins change.
#define DEFINE_POWERDOWN_GPIO_CFG_VARIABLES()                                     \
  uint16_t gpioCfgPowerDown[6] = {                                                \
    ((GPIOCFG_OUT       << PA0_CFG_BIT)                                           \
     | (GPIOCFG_OUT_ALT << PA1_CFG_BIT)                                           \
     | (GPIOCFG_IN      << PA2_CFG_BIT)                                           \
     | (GPIOCFG_OUT     << PA3_CFG_BIT)),                                         \
    ((GPIOCFG_OUT_ALT   << PA4_CFG_BIT)                                           \
     | (GPIOCFG_OUT_ALT << PA5_CFG_BIT)                                           \
     | (GPIOCFG_OUT     << PA6_CFG_BIT)                                           \
     | (GPIOCFG_OUT     << PA7_CFG_BIT)),                                         \
    ((GPIOCFG_IN_PUD    << PB0_CFG_BIT)                                           \
     | (GPIOCFG_OUT_ALT << PB1_CFG_BIT)                                           \
     | (GPIOCFG_IN      << PB2_CFG_BIT)                                           \
     | (GPIOCFG_IN      << PB3_CFG_BIT)),                                         \
    ((GPIOCFG_IN_PUD    << PB4_CFG_BIT)                                           \
     | (GPIOCFG_IN      << PB5_CFG_BIT)                                           \
     | (GPIOCFG_IN_PUD  << PB6_CFG_BIT)                                           \
     | (GPIOCFG_OUT     << PB7_CFG_BIT)),                                         \
    ((GPIOCFG_OUT_ALT   << PC0_CFG_BIT)                                           \
     | (GPIOCFG_OUT     << PC1_CFG_BIT)                                           \
     | (GPIOCFG_OUT_ALT << PC2_CFG_BIT)                                           \
     | (GPIOCFG_IN      << PC3_CFG_BIT)),                                         \
    ((GPIOCFG_IN        << PC4_CFG_BIT)                                           \
     | (GPIOCFG_OUT     << PC5_CFG_BIT)                                           \
     | (GPIOCFG_OUT     << PC6_CFG_BIT)                                           \
     | (GPIOCFG_IN      << PC7_CFG_BIT))                                          \
  }

#define DEFINE_POWERDOWN_GPIO_OUTPUT_DATA_VARIABLES() \
  uint8_t gpioOutPowerDown[3] = { 0xE3, 0xDF, 0x05 }

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

#define CONFIGURE_EXTERNAL_REGULATOR_ENABLE() GPIO_DBGCFG |= GPIO_EXTREGEN;

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
    /* The radio's pins stay powered down until the stack powers it up. */ \
    halStackRadioPowerDownBoard();                 \
    CONFIGURE_EXTERNAL_REGULATOR_ENABLE()          \
    halHostSerialPowerup();                        \
    halInternalInitAdc();                          \
  } while (0)

#define halInternalSuspendBoard() do {} while (0)
#define halInternalResumeBoard()  do {} while (0)

#endif // __BOARD_H__
