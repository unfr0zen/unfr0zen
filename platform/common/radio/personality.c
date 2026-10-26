// One image, two personalities (or the RCP alone, RADIO_PERSONALITY_thread): the EmberZNet NCP (zigbeeMain, the SDK's
// em260 main) or the OpenThread RCP (threadMain, thread/rcp.c). The radio
// boots as the RCP; the RCP's SPI command 6 asks for Zigbee, which is
// answered with a reboot into the NCP. The request lives in the reset-info
// RAM segment, which startup leaves alone, and is consumed by the one boot
// it names: a hardware reset brings the RCP back.

#include PLATFORM_HEADER
#include "hal/hal.h"

#define ZIGBEE_REQUESTED 0x5a424545u

VAR_AT_SEGMENT(NO_STRIPPING static uint32_t personalityRequest, __RESETINFO__);

bool radioZigbee;

extern int zigbeeMain(void);
extern int threadMain(void);

#if defined(RADIO_PERSONALITY_thread)
// The RCP alone (the EM357 holds one personality): Zigbee is another image,
// so a request for it changes nothing.
int main(void)
{
  personalityRequest = 0;
  return threadMain();
}

void radioSelectZigbee(void)
{
}
#else
int main(void)
{
  radioZigbee = personalityRequest == ZIGBEE_REQUESTED;
  personalityRequest = 0;
  return radioZigbee ? zigbeeMain() : threadMain();
}

void radioSelectZigbee(void)
{
  personalityRequest = ZIGBEE_REQUESTED;
  halReboot();
}
#endif
