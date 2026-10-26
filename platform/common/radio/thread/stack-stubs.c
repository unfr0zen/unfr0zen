/* The stack's application callbacks, for the RCP alone (the EM357's Thread
   image): with no NCP and no Zigbee network, none of them ever has anything
   to do. The RCP drives the PHY directly, past the network layer that calls
   these. */
#include PLATFORM_HEADER
#include "stack/include/ember.h"

EmberPacketAction emberPacketHandoffIncoming(EmberZigbeePacketType packetType, EmberMessageBuffer packetBuffer,
                                             uint8_t index, void *data) { return EMBER_ACCEPT_PACKET; }
EmberPacketAction emberPacketHandoffOutgoing(EmberZigbeePacketType packetType, EmberMessageBuffer packetBuffer,
                                             uint8_t index, void *data) { return EMBER_ACCEPT_PACKET; }
uint8_t emberAppendSourceRouteHandler(EmberNodeId destination, EmberMessageBuffer header) { return 0; }
void emberRtosStackWakeupIsrHandler(void) {}
bool emberRtosIdleHandler(uint32_t *idleTimeMs) { return false; }
void emberMessageSentHandler(EmberOutgoingMessageType type, uint16_t indexOrDestination, EmberApsFrame *apsFrame,
                             EmberMessageBuffer message, EmberStatus status) {}
void emberChangeSourceRouteHandler(EmberNodeId newChildId, EmberNodeId newParentId, bool ourChild, bool deviceLeft) {}
EmberJoinDecision emberTrustCenterJoinHandler(EmberNodeId newNodeId, EmberEUI64 newNodeEui64, EmberDeviceUpdate status,
                                              EmberNodeId parentOfNewNode) { return EMBER_DENY_JOIN; }
EmberZdoStatus emberRemoteDeleteBindingHandler(uint8_t index) { return EMBER_ZDP_NOT_SUPPORTED; }
EmberZdoStatus emberRemoteSetBindingHandler(EmberBindingTableEntry *entry) { return EMBER_ZDP_NOT_SUPPORTED; }
void emberIncomingRouteRecordHandler(EmberNodeId source, EmberEUI64 sourceEui, uint8_t relayCount,
                                     EmberMessageBuffer header, uint8_t relayListIndex) {}
uint8_t emberEndpointCount = 0;
bool emberGetEndpointDescription(uint8_t endpoint, EmberEndpointDescription *result) { return false; }
uint16_t emberGetEndpointCluster(uint8_t endpoint, EmberClusterListId listId, uint8_t listIndex) { return 0; }
uint8_t emberGetEndpoint(uint8_t index) { return 0xFF; }
void emberCounterHandler(EmberCounterType type, EmberCounterInfo info) {}
void emberZigbeeKeyEstablishmentHandler(EmberEUI64 partner, EmberKeyStatus status) {}
void emberUnusedPanIdFoundHandler(EmberPanId panId, uint8_t channel) {}
void emberSwitchNetworkKeyHandler(uint8_t sequenceNumber) {}
void emberStackTokenChangedHandler(uint16_t tokenAddress) {}
void emberStackStatusHandler(EmberStatus status) {}
void emberScanCompleteHandler(uint8_t channel, EmberStatus status) {}
void emberRawTransmitCompleteHandler(EmberMessageBuffer message, EmberStatus status) {}
void emberRadioNeedsCalibratingHandler(void) { emberCalibrateCurrentChannel(); }
void emberPollHandler(EmberNodeId childId, bool transmitExpected) {}
void emberPollCompleteHandler(EmberStatus status) {}
EmberStatus emberPanIdConflictHandler(int8_t conflictCount) { return EMBER_SUCCESS; }
void emberOrphanNotificationHandler(EmberEUI64 longId) {}
void emberNetworkFoundHandler(EmberZigbeeNetwork *networkFound) {}
bool emberMacPassthroughFilterHandler(uint8_t *macHeader) { return false; }
void emberMacFilterMatchMessageHandler(const EmberMacFilterMatchStruct *macFilterMatchStruct) {}
void emberIncomingRouteErrorHandler(EmberStatus status, EmberNodeId target) {}
void emberIncomingMessageHandler(EmberIncomingMessageType type, EmberApsFrame *apsFrame, EmberMessageBuffer message) {}
void emberIncomingBootloadMessageHandler(EmberEUI64 longId, EmberMessageBuffer message) {}
void emberIdConflictHandler(EmberNodeId conflictingId) {}
void emberEnergyScanResultHandler(uint8_t channel, int8_t maxRssiValue) {}
void emberChildJoinHandler(uint8_t index, bool joining) {}
void emberBootloadTransmitCompleteHandler(EmberMessageBuffer message, EmberStatus status) {}
/* The Makefile wraps the many-to-one handler for the NCP; here it is the stub. */
void __wrap_emberIncomingManyToOneRouteRequestHandler(EmberNodeId source, EmberEUI64 longId, uint8_t cost) {}
