/* OpenThread Spinel RCP over the Nest SPI link. */
#include PLATFORM_HEADER
#include "stack/include/ember.h"
#include "hal/hal.h"
#include "hal/micro/cortexm3/spi-protocol.h"
#include <openthread/instance.h>
#include <openthread/ncp.h>
#include <openthread/tasklet.h>
#include <openthread/platform/alarm-milli.h>
#include <openthread/platform/alarm-micro.h>
#include <openthread/platform/entropy.h>
#include <openthread/platform/misc.h>
#include <openthread/platform/radio.h>
#include <openthread/platform/time.h>

extern bool halHostSerialTick(bool responseReady);
extern void emRadioSetPanId(uint16_t);
extern void emRadioSetNodeId(uint16_t);
extern void emRadioEnableAddressMatching(uint8_t);
extern void emRadioEnableAutoAck(bool);
extern void emRadioWakeUp(void);
extern void emRadioSleep(void);
extern EmberStatus emRadioTransmit(uint8_t *);
extern void emSetPhyRadioChannel(uint8_t);
extern void emSetPhyRadioPower(int8_t);
extern void emPhySetEui64(void);
extern uint8_t emLocalEui64[8];
extern int8_t emRadioEnergyDetection(void);
extern int8_t emRadioGetEdCcaThreshold(void);
extern void emRadioSetEdCcaThreshold(int8_t);
extern const char rcp_identity[];
extern uint16_t *emChildStatus;
extern const uint16_t *threadRadioChildStatus;
extern bool radioZigbee;
extern void radioSelectZigbee(void);
extern void __real_emRadioTransmitCompleteCallback(EmberStatus status, uint32_t time, uint8_t pending);
extern void __real_emPacketReceivedInternalIsrCallback(const uint8_t *data, uint8_t allocated, uint8_t flags);
extern void __real_halInternalAssertFailed(const char *file, int line);
extern void halHardFaultIsr(void);
extern bool halHostSerialBusy(void);

static otInstance *instance;
/* The stack's EUI-64 becomes the host's extended address; the factory one stays here. */
static uint8_t factory_eui64[8];
static otRadioState radio_state = OT_RADIO_STATE_DISABLED;
static uint8_t channel = 11;
static int8_t power = -15;
static bool promiscuous;
static uint8_t phy_tx[132] __attribute__((aligned(4)));
static uint8_t ack_psdu[5];
static otRadioFrame tx_frame, rx_frame, ack_frame;
static volatile bool tx_complete;
static volatile uint8_t tx_result, tx_pending;
static const uint8_t *serial_output;
static uint16_t serial_length, serial_offset;
static bool host_alarm_active, host_alarm_pending;
static uint32_t host_alarm_at;
static bool alarm_active;
static uint32_t alarm_at;
static bool micro_alarm_active;
static uint32_t micro_alarm_at, last_cycles;
static uint64_t elapsed_cycles;
static bool energy_scan;
static uint32_t energy_scan_start, energy_scan_duration;
static int8_t energy_scan_max;
static otRadioState energy_scan_restore;

static int serial_send(const uint8_t *data, uint16_t length) {
  if (serial_length) return 0;
  serial_output = data;
  serial_length = length;
  halHostCallback(true);
  serial_offset = 0;
  return length;
}

void __wrap_emRadioTransmitCompleteCallback(EmberStatus status, uint32_t time, uint8_t pending) {
  if (radioZigbee) { __real_emRadioTransmitCompleteCallback(status, time, pending); return; }
  tx_result = status;
  tx_pending = pending;
  tx_complete = true;
}

/* The PHY callback runs in the receive ISR after CRC validation. */
#define RX_QUEUE_SIZE 8
static struct {
  uint8_t psdu[127], length, channel;
  bool ack_pending;
  int8_t rssi;
} rx_queue[RX_QUEUE_SIZE];
static volatile uint8_t rx_read, rx_write;

void __wrap_emPacketReceivedInternalIsrCallback(const uint8_t *data, uint8_t allocated, uint8_t flags) {
  if (radioZigbee) { __real_emPacketReceivedInternalIsrCallback(data, allocated, flags); return; }
  if (energy_scan) return;
  uint8_t length = data[0], next = (rx_write + 1) % RX_QUEUE_SIZE;
  if (length < 5 || length > 127 || allocated < length + 1 || next == rx_read) return;
  halCommonMemMove(rx_queue[rx_write].psdu, data + 1, length - 2);
  rx_queue[rx_write].psdu[length - 2] = rx_queue[rx_write].psdu[length - 1] = 0;
  rx_queue[rx_write].length = length;
  rx_queue[rx_write].channel = channel;
  /* PHY bit 0 records pending in the automatic ACK, not the incoming frame. */
  rx_queue[rx_write].ack_pending = (flags & 1) != 0;
  /* The PHY converts RSSI in the trailing eight-byte DMA metadata to dBm. */
  rx_queue[rx_write].rssi = (int8_t)data[allocated - 6];
  __DMB();
  rx_write = next;
}

static void receive_pending(void) {
  while (rx_read != rx_write) {
    halCommonMemSet(&rx_frame, 0, sizeof(rx_frame));
    rx_frame.mPsdu = rx_queue[rx_read].psdu;
    rx_frame.mLength = rx_queue[rx_read].length;
    rx_frame.mChannel = rx_queue[rx_read].channel;
    rx_frame.mInfo.mRxInfo.mRssi = rx_queue[rx_read].rssi;
    rx_frame.mInfo.mRxInfo.mAckedWithFramePending = rx_queue[rx_read].ack_pending;
    otPlatRadioReceiveDone(instance, &rx_frame, OT_ERROR_NONE);
    __DMB();
    rx_read = (rx_read + 1) % RX_QUEUE_SIZE;
  }
}

void otPlatRadioGetIeeeEui64(otInstance *i, uint8_t *address) {
  for (uint8_t n = 0; n < 8; n++) address[n] = factory_eui64[7 - n];
}
void otPlatRadioSetPanId(otInstance *i, otPanId pan) { emRadioSetPanId(pan); }
void otPlatRadioSetShortAddress(otInstance *i, otShortAddress address) { emRadioSetNodeId(address); }
void otPlatRadioSetExtendedAddress(otInstance *i, const otExtAddress *address) {
  halCommonMemMove(emLocalEui64, address->m8, 8);
  emPhySetEui64();
}
void otPlatRadioSetPromiscuous(otInstance *i, bool enabled) {
  promiscuous = enabled;
  emRadioEnableAddressMatching(!enabled);
}
bool otPlatRadioGetPromiscuous(otInstance *i) { return promiscuous; }
bool otPlatRadioIsEnabled(otInstance *i) { return radio_state != OT_RADIO_STATE_DISABLED; }
otError otPlatRadioEnable(otInstance *i) {
  radio_state = OT_RADIO_STATE_SLEEP;
  return OT_ERROR_NONE;
}
otError otPlatRadioDisable(otInstance *i) {
  if (energy_scan) return OT_ERROR_BUSY;
  emRadioSleep(); radio_state = OT_RADIO_STATE_DISABLED; return OT_ERROR_NONE;
}
otError otPlatRadioSleep(otInstance *i) {
  if (energy_scan || radio_state == OT_RADIO_STATE_TRANSMIT) return OT_ERROR_BUSY;
  emRadioSleep(); radio_state = OT_RADIO_STATE_SLEEP; return OT_ERROR_NONE;
}
otError otPlatRadioReceive(otInstance *i, uint8_t value) {
  if (energy_scan) return OT_ERROR_BUSY;
  channel = value; emSetPhyRadioChannel(channel); emRadioWakeUp();
  radio_state = OT_RADIO_STATE_RECEIVE; return OT_ERROR_NONE;
}
otRadioFrame *otPlatRadioGetTransmitBuffer(otInstance *i) { return &tx_frame; }
otError otPlatRadioTransmit(otInstance *i, otRadioFrame *frame) {
  if (energy_scan) return OT_ERROR_BUSY;
  if (frame->mLength < 5 || frame->mLength > 127) return OT_ERROR_INVALID_ARGS;
  emSetPhyRadioChannel(frame->mChannel);
  phy_tx[0] = frame->mLength;
  tx_complete = false;
  EmberStatus result = emRadioTransmit(phy_tx);
  if (result != EMBER_SUCCESS) return OT_ERROR_BUSY;
  radio_state = OT_RADIO_STATE_TRANSMIT;
  otPlatRadioTxStarted(i, frame);
  return OT_ERROR_NONE;
}
int8_t otPlatRadioGetRssi(otInstance *i) {
  return radio_state == OT_RADIO_STATE_RECEIVE ? emRadioEnergyDetection() : OT_RADIO_RSSI_INVALID;
}
otRadioCaps otPlatRadioGetCaps(otInstance *i) { return OT_RADIO_CAPS_ACK_TIMEOUT | OT_RADIO_CAPS_ENERGY_SCAN; }
otRadioState otPlatRadioGetState(otInstance *i) { return radio_state; }
int8_t otPlatRadioGetReceiveSensitivity(otInstance *i) { return -100; }
otError otPlatRadioGetTransmitPower(otInstance *i, int8_t *value) { *value = power; return OT_ERROR_NONE; }
otError otPlatRadioSetTransmitPower(otInstance *i, int8_t value) {
#ifdef RADIO_TX_POWER_MAX
  if (value > RADIO_TX_POWER_MAX) value = RADIO_TX_POWER_MAX;
#endif
  power = value; emSetPhyRadioPower(value); return OT_ERROR_NONE;
}
otError otPlatRadioEnergyScan(otInstance *i, uint8_t c, uint16_t duration) {
  if (c < 11 || c > 26 || !duration) return OT_ERROR_INVALID_ARGS;
  if (radio_state == OT_RADIO_STATE_DISABLED) return OT_ERROR_INVALID_STATE;
  if (energy_scan || radio_state == OT_RADIO_STATE_TRANSMIT) return OT_ERROR_BUSY;
  energy_scan_restore = radio_state;
  energy_scan_max = -128;
  energy_scan = true;
  emRadioEnableAutoAck(false);
  emSetPhyRadioChannel(c);
  emRadioWakeUp();
  radio_state = OT_RADIO_STATE_RECEIVE;
  energy_scan_start = otPlatAlarmMicroGetNow();
  energy_scan_duration = (uint32_t)duration * 1000;
  return OT_ERROR_NONE;
}

/* Sampling starts after eight symbols of receiver settling; SPI remains live. */
void threadRadioEnergyScanTick(void) {
  if (!energy_scan) return;
  uint32_t elapsed = otPlatAlarmMicroGetNow() - energy_scan_start;
  if (elapsed < 128) return;
  int8_t sample = emRadioEnergyDetection();
  if (sample > energy_scan_max) energy_scan_max = sample;
  if (elapsed < energy_scan_duration) return;
  emSetPhyRadioChannel(channel);
  if (energy_scan_restore == OT_RADIO_STATE_SLEEP) emRadioSleep();
  emRadioEnableAutoAck(true);
  radio_state = energy_scan_restore;
  energy_scan = false;
  otPlatRadioEnergyScanDone(instance, energy_scan_max);
}

otError otPlatRadioGetCcaEnergyDetectThreshold(otInstance *i, int8_t *value) {
  if (!value) return OT_ERROR_INVALID_ARGS;
  *value = emRadioGetEdCcaThreshold(); return OT_ERROR_NONE;
}
otError otPlatRadioSetCcaEnergyDetectThreshold(otInstance *i, int8_t value) {
  if (value > 0) return OT_ERROR_INVALID_ARGS;
  emRadioSetEdCcaThreshold(value); return OT_ERROR_NONE;
}
uint32_t otPlatAlarmMilliGetNow(void) { return halCommonGetInt32uMillisecondTick(); }
void otPlatAlarmMilliStartAt(otInstance *i, uint32_t t0, uint32_t dt) { alarm_at = t0 + dt; alarm_active = true; }
void otPlatAlarmMilliStop(otInstance *i) { alarm_active = false; }
uint64_t otPlatTimeGet(void) {
  /* DWT runs at the 24 MHz core clock. The awake main loop extends its wrap. */
  uint32_t cycles = *(volatile uint32_t *)0xe0001004;
  elapsed_cycles += (uint32_t)(cycles - last_cycles);
  last_cycles = cycles;
  return elapsed_cycles / 24;
}
uint32_t otPlatAlarmMicroGetNow(void) { return (uint32_t)otPlatTimeGet(); }
void otPlatAlarmMicroStartAt(otInstance *i, uint32_t t0, uint32_t dt) { micro_alarm_at = t0 + dt; micro_alarm_active = true; }
void otPlatAlarmMicroStop(otInstance *i) { micro_alarm_active = false; }
uint16_t otPlatTimeGetXtalAccuracy(void) { return 100; }
void otPlatWakeHost(void) {}
void otTaskletsSignalPending(otInstance *i) {}
otError otPlatEntropyGet(uint8_t *out, uint16_t length) {
  if (!out) return OT_ERROR_INVALID_ARGS;
  while (length) {
    uint16_t random;
    if (emberGetStrongRandomNumberArray(&random, 1) != EMBER_SUCCESS) return OT_ERROR_FAILED;
    *out++ = random;
    length--;
    if (length) { *out++ = random >> 8; length--; }
  }
  return OT_ERROR_NONE;
}
void otPlatReset(otInstance *i) { halReboot(); }
otPlatResetReason otPlatGetResetReason(otInstance *i) { return OT_PLAT_RESET_REASON_POWER_ON; }
static void report_fault(const char *file, int line) {
  uint8_t report[100];
  report[0] = 0x53; report[1] = 0xee; report[2] = line; report[3] = line >> 8;
  uint8_t length = 4;
  while (*file && length < sizeof(report)) report[length++] = *file++;
  INTERRUPTS_ON();
  while (true) {
    halResetWatchdog();
    if (halHostSerialTick(false)) {
      halHostFrame[0] = length;
      halCommonMemMove(halHostFrame + 1, report, length);
      halHostSerialTick(true);
    }
  }
}
static uint32_t fault_pc, fault_lr, fault_cfsr;
static void fault_entry(void) {
  char message[48] = "HardFault pc=00000000 lr=00000000 cfsr=00000000";
  const char hex[] = "0123456789abcdef";
  for (uint8_t n = 0; n < 8; n++) {
    message[13 + n] = hex[(fault_pc >> (28 - n * 4)) & 15];
    message[25 + n] = hex[(fault_lr >> (28 - n * 4)) & 15];
    message[39 + n] = hex[(fault_cfsr >> (28 - n * 4)) & 15];
  }
  report_fault(message, 0);
}
__attribute__((used)) void capture_fault(uint32_t *stack) {
  fault_pc = stack[6]; fault_lr = stack[5];
  fault_cfsr = *(volatile uint32_t *)0xe000ed28;
  *(volatile uint32_t *)0xe000ed28 = fault_cfsr;
  stack[6] = (uint32_t)fault_entry;
  stack[7] = 0x01000000;
}
/* The Zigbee personality keeps the SDK's crash handling. */
__attribute__((naked)) void HardFault_Handler(void) {
  __asm volatile ("ldr r0, =radioZigbee\nldrb r0, [r0]\ncbz r0, 1f\nb halHardFaultIsr\n"
                  "1:\ntst lr, #4\nite eq\nmrseq r0, msp\nmrsne r0, psp\nb capture_fault\n");
}
void otPlatAssertFail(const char *file, int line) { report_fault(file, line); }
void __wrap_halInternalAssertFailed(const char *file, int line) {
  if (radioZigbee) __real_halInternalAssertFailed(file, line);
  report_fault(file, line);
}
void __assert_func(const char *file, int line, const char *function, const char *assertion) { report_fault(file, line); }

int threadMain(void) {
  halInit(); INTERRUPTS_ON();
  EmberStatus init_status = emberInit();
  halCommonMemMove(factory_eui64, emLocalEui64, 8);
  /* The PHY's early-ACK lookup reads the stack's child status table; this
   * personality never joins a Zigbee network, so it reads Thread's instead. */
  emChildStatus = (uint16_t *)threadRadioChildStatus;
  emberSetTxPowerMode(RADIO_TX_POWER_MODE);
  *(volatile uint32_t *)0xe000edfc |= 1UL << 24; /* DEMCR.TRCENA */
  *(volatile uint32_t *)0xe0001004 = 0; /* DWT.CYCCNT */
  *(volatile uint32_t *)0xe0001000 |= 1; /* DWT.CTRL.CYCCNTENA */
  emRadioEnableAutoAck(true);
  tx_frame.mPsdu = phy_tx + 1;
  instance = otInstanceInitSingle();
  otNcpHdlcInit(instance, serial_send);
  bool zigbee_requested = false;
  while (true) {
    halResetWatchdog();
    if (zigbee_requested && !halHostSerialBusy()) radioSelectZigbee();
    emberTick();
    receive_pending();
    otTaskletsProcess(instance);
    uint32_t now_us = otPlatAlarmMicroGetNow();
    threadRadioEnergyScanTick();
    if (micro_alarm_active && (int32_t)(now_us - micro_alarm_at) >= 0) {
      micro_alarm_active = false; otPlatAlarmMicroFired(instance);
    }
    if (alarm_active && (int32_t)(otPlatAlarmMilliGetNow() - alarm_at) >= 0) {
      alarm_active = false; otPlatAlarmMilliFired(instance);
    }
    if (host_alarm_active && (int32_t)(otPlatAlarmMilliGetNow() - host_alarm_at) >= 0) {
      host_alarm_active = false;
      host_alarm_pending = true;
      halHostCallback(true);
    }
    if (tx_complete) {
      tx_complete = false;
      radio_state = OT_RADIO_STATE_RECEIVE;
      otError error = (tx_result == EMBER_PHY_ACK_RECEIVED || (tx_result == EMBER_SUCCESS && !(phy_tx[1] & 0x20))) ? OT_ERROR_NONE : tx_result == EMBER_SUCCESS ? OT_ERROR_NO_ACK : OT_ERROR_CHANNEL_ACCESS_FAILURE;
      otRadioFrame *ack = NULL;
      if (error == OT_ERROR_NONE && (phy_tx[1] & 0x20)) {
        ack_psdu[0] = 2 | (tx_pending ? 0x10 : 0); ack_psdu[1] = 0; ack_psdu[2] = phy_tx[3];
        ack_frame.mPsdu = ack_psdu; ack_frame.mLength = 5; ack_frame.mChannel = channel; ack = &ack_frame;
      }
      otPlatRadioTxDone(instance, &tx_frame, ack, error);
    }
    if (halHostSerialTick(false)) {
      uint8_t command = halHostFrame[0] ? halHostFrame[1] : 0;
      if (command == 1 && halHostFrame[0] > 1) otNcpHdlcReceive(halHostFrame + 2, halHostFrame[0] - 1);
      if (command == 3 && halHostFrame[0] == 5) {
        uint32_t delay = (uint32_t)halHostFrame[2] | ((uint32_t)halHostFrame[3] << 8)
          | ((uint32_t)halHostFrame[4] << 16) | ((uint32_t)halHostFrame[5] << 24);
        host_alarm_at = otPlatAlarmMilliGetNow() + delay;
        host_alarm_active = delay != 0;
        host_alarm_pending = false;
      }
      halHostFrame[0] = 3; halHostFrame[1] = 0x52; halHostFrame[2] = 2; halHostFrame[3] = init_status;
      if (command == 4) {
        halHostFrame[0] = 65;
        halCommonMemMove(halHostFrame + 2, rcp_identity, 64);
      }
      /* Zigbee is selected once this answer has left. */
      if (command == 6) zigbee_requested = true;
      bool done = false;
      if (command == 2) {
        if (host_alarm_pending) halHostFrame[1] = 0x54;
        host_alarm_pending = false;
        uint16_t length = serial_length ? serial_length - serial_offset : 0;
        if (length > 116) length = 116;
        halHostFrame[0] = 1 + length;
        if (length) halCommonMemMove(halHostFrame + 2, serial_output + serial_offset, length);
        serial_offset += length;
        done = serial_length && serial_offset == serial_length;
      }
      if (done) { serial_length = serial_offset = 0; }
      halHostCallback(serial_length != 0 || host_alarm_pending);
      halHostSerialTick(true);
      if (done) otNcpHdlcSendDone();
    }
  }
}
