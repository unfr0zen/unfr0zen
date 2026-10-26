// What AppBuilder generates for an NCP-SPI build with the PRO, security and
// binding EZSP plugins: the chains that let each plugin answer value, policy
// and configuration frames, the event table, and the callbacks left empty.

#include PLATFORM_HEADER
#include "stack/include/ember.h"
#include "app/util/ezsp/ezsp-protocol.h"
#include "app/em260/command-context.h"
#include "app/util/ezsp/ezsp-frame-utilities.h"
#include "hal/hal.h"
#include "serial/serial.h"
#include "sim-eeprom/sim-eeprom.h"

// A coordinator that is a concentrator, as Zigbee2MQTT's is, broadcasts a
// many-to-one route request every minute or so. The stack keeps the route
// itself; the library's handler would also pass each request to the host,
// which a router's host has no use for, and the callback would wake the
// sleeping unit for it. The Makefile wraps the handler with this.
void __wrap_emberIncomingManyToOneRouteRequestHandler(EmberNodeId source, EmberEUI64 longId, uint8_t cost)
{
  (void)source;
  (void)longId;
  (void)cost;
}

// One plugin after another until one consumes the frame.
#define CHAIN(context, ...)                               \
  do {                                                    \
    void (*const handlers[])(typeof(context)) = { __VA_ARGS__ }; \
    for (uint8_t i = 0; i < COUNTOF(handlers) && !(context)->consumed; i++) { \
      handlers[i](context);                               \
    }                                                     \
  } while (0)

void emberAfPluginEzspGetValueCommandCallback(EmberAfPluginEzspValueCommandContext *context);
void emberAfPluginEzspSetValueCommandCallback(EmberAfPluginEzspValueCommandContext *context);
void emberAfPluginEzspPolicyCommandCallback(EmberAfPluginEzspPolicyCommandContext *context);
void emberAfPluginEzspGetConfigurationValueCommandCallback(EmberAfPluginEzspConfigurationValueCommandContext *context);
void emberAfPluginEzspSetConfigurationValueCommandCallback(EmberAfPluginEzspConfigurationValueCommandContext *context);
void emberAfPluginEzspSecurityGetValueCommandCallback(EmberAfPluginEzspValueCommandContext *context);
void emberAfPluginEzspSecuritySetValueCommandCallback(EmberAfPluginEzspValueCommandContext *context);
void emberAfPluginEzspSecurityPolicyCommandCallback(EmberAfPluginEzspPolicyCommandContext *context);
void emberAfPluginEzspSecurityGetConfigurationValueCommandCallback(EmberAfPluginEzspConfigurationValueCommandContext *context);
void emberAfPluginEzspSecuritySetConfigurationValueCommandCallback(EmberAfPluginEzspConfigurationValueCommandContext *context);
void emberAfPluginEzspZigbeeProGetValueCommandCallback(EmberAfPluginEzspValueCommandContext *context);
void emberAfPluginEzspZigbeeProGetExtendedValueCommandCallback(EmberAfPluginEzspValueCommandContext *context);
void emberAfPluginEzspZigbeeProSetValueCommandCallback(EmberAfPluginEzspValueCommandContext *context);
void emberAfPluginEzspZigbeeProPolicyCommandCallback(EmberAfPluginEzspPolicyCommandContext *context);
void emberAfPluginEzspZigbeeProGetConfigurationValueCommandCallback(EmberAfPluginEzspConfigurationValueCommandContext *context);
void emberAfPluginEzspZigbeeProSetConfigurationValueCommandCallback(EmberAfPluginEzspConfigurationValueCommandContext *context);
void emberAfPluginEzspZigbeeProModifyMemoryAllocationCallback(void);
void emberAfPluginEzspBindingPolicyCommandCallback(EmberAfPluginEzspPolicyCommandContext *context);
void emberAfPluginEzspBindingConfigurationValueCommandCallback(EmberAfPluginEzspConfigurationValueCommandContext *context);
void emberAfPluginEzspBindingPermitHostToNcpFrameCallback(bool *permit);
void emInitializeBindingTable(void);

void emberAfEzspGetValueCommandCallback(EmberAfPluginEzspValueCommandContext *context)
{
  CHAIN(context, emberAfPluginEzspGetValueCommandCallback,
        emberAfPluginEzspSecurityGetValueCommandCallback,
        emberAfPluginEzspZigbeeProGetValueCommandCallback);
}

void emberAfEzspGetExtendedValueCommandCallback(EmberAfPluginEzspValueCommandContext *context)
{
  CHAIN(context, emberAfPluginEzspZigbeeProGetExtendedValueCommandCallback);
}

void emberAfEzspSetValueCommandCallback(EmberAfPluginEzspValueCommandContext *context)
{
  CHAIN(context, emberAfPluginEzspSetValueCommandCallback,
        emberAfPluginEzspSecuritySetValueCommandCallback,
        emberAfPluginEzspZigbeeProSetValueCommandCallback);
}

void emberAfEzspGetPolicyCommandCallback(EmberAfPluginEzspPolicyCommandContext *context)
{
  CHAIN(context, emberAfPluginEzspPolicyCommandCallback,
        emberAfPluginEzspBindingPolicyCommandCallback,
        emberAfPluginEzspSecurityPolicyCommandCallback,
        emberAfPluginEzspZigbeeProPolicyCommandCallback);
}

void emberAfEzspSetPolicyCommandCallback(EmberAfPluginEzspPolicyCommandContext *context)
{
  emberAfEzspGetPolicyCommandCallback(context);
}

void emberAfEzspGetConfigurationValueCommandCallback(EmberAfPluginEzspConfigurationValueCommandContext *context)
{
  CHAIN(context, emberAfPluginEzspGetConfigurationValueCommandCallback,
        emberAfPluginEzspBindingConfigurationValueCommandCallback,
        emberAfPluginEzspSecurityGetConfigurationValueCommandCallback,
        emberAfPluginEzspZigbeeProGetConfigurationValueCommandCallback);
}

void emberAfEzspSetConfigurationValueCommandCallback(EmberAfPluginEzspConfigurationValueCommandContext *context)
{
  CHAIN(context, emberAfPluginEzspSetConfigurationValueCommandCallback,
        emberAfPluginEzspBindingConfigurationValueCommandCallback,
        emberAfPluginEzspSecuritySetConfigurationValueCommandCallback,
        emberAfPluginEzspZigbeeProSetConfigurationValueCommandCallback);
}

void emberAfPluginEzspModifyMemoryAllocationCallback(void)
{
  emInitializeBindingTable();
  emberAfPluginEzspZigbeeProModifyMemoryAllocationCallback();
}

void emberAfPluginEzspPermitHostToNcpFrameCallback(bool *permit)
{
  emberAfPluginEzspBindingPermitHostToNcpFrameCallback(permit);
}

void emberAfInit(void)
{
}

void emberAfTick(void)
{
}

// The plugins' timers.
#define EVENTS(X)                           \
  X(emberAfPluginEzspHostTimer0)            \
  X(emberAfPluginEzspHostTimer1)            \
  X(emberAfPluginEzspZigbeeProPollTimer0)   \
  X(emberAfPluginEzspZigbeeProPollTimer1)   \
  X(emberAfPluginEzspZigbeeProPollTimer2)   \
  X(emberAfPluginEzspZigbeeProPollTimer3)
#define DECLARE(name) extern EmberEventControl name##EventControl; void name##EventHandler(void);
#define ENTRY(name) { &name##EventControl, name##EventHandler },
EVENTS(DECLARE)
EmberEventData emAppEvents[] = {
  EVENTS(ENTRY)
  { NULL, NULL }
};

// XNCP, in place of its stub library: no custom frames, and the image's
// version as the XNCP version, for nestlocal to tell its own image.
bool emXNcpAllowIncomingEzspCommand(void)
{
  return true;
}

bool emXNcpAllowOutgoingEzspCallback(void)
{
  return true;
}

bool emAfPluginXncpIncomingMessageCallback(void)
{
  return false;
}

void emXNcpHandleIncomingCustomEzspMessage(void)
{
  appendInt8u(EMBER_INVALID_CALL);
  appendInt8u(0); // no reply
}

void emXNcpHandleGetInfoCommand(void)
{
  appendInt8u(EMBER_SUCCESS);
  appendInt16u(0); // manufacturer
  appendInt16u(CUSTOMER_APPLICATION_VERSION);
}

// The callbacks no plugin implements, as AppBuilder generates them.
void emberRadioNeedsCalibratingHandler(void)
{
  emberCalibrateCurrentChannel();
}

EmberJoinDecision emberAfPluginEzspSecurityTrustCenterJoinCallback(EmberNodeId newNodeId,
                                                                   const EmberEUI64 newNodeEui64,
                                                                   EmberDeviceUpdate status,
                                                                   EmberNodeId parentOfNewNode,
                                                                   EzspDecisionId decisionId,
                                                                   EmberJoinDecision joinDecision)
{
  return joinDecision;
}

// A router is never a concentrator.
EmberStatus emberAfPluginEzspZigbeeProSetConcentratorCommandCallback(bool on, uint16_t concentratorType,
                                                                     uint16_t minTime, uint16_t maxTime,
                                                                     uint8_t routeErrorThreshold,
                                                                     uint8_t deliveryFailureThreshold,
                                                                     uint8_t maxHops)
{
  return EMBER_INVALID_CALL;
}

void emberSetOrGetEzspTokenCommandHandler(bool isSet)
{
  appendInt8u(EMBER_INVALID_CALL);
}

void emberAfCounterHandler(EmberCounterType type, EmberCounterInfo info)
{
}

EmberStatus emberPanIdConflictHandler(int8_t conflictCount)
{
  return EMBER_SUCCESS;
}

void emberOrphanNotificationHandler(EmberEUI64 longId)
{
}

bool emberMacPassthroughFilterHandler(uint8_t *macHeader)
{
  return false;
}

EmberPacketAction emberPacketHandoffIncoming(EmberZigbeePacketType packetType,
                                             EmberMessageBuffer packetBuffer,
                                             uint8_t index, void *data)
{
  return EMBER_ACCEPT_PACKET;
}

EmberPacketAction emberPacketHandoffOutgoing(EmberZigbeePacketType packetType,
                                             EmberMessageBuffer packetBuffer,
                                             uint8_t index, void *data)
{
  return EMBER_ACCEPT_PACKET;
}

bool emberRtosIdleHandler(uint32_t *idleTimeMs)
{
  return false;
}

void emberRtosStackWakeupIsrHandler(void)
{
}
