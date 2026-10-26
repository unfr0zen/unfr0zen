// Radio coexistence (PTA) is not wired on the Nest, and the SDK's plugin is not
// part of Gecko SDK 2.6's base platform: just what micro.c calls. ncp.c has
// the stubs the stack links against.

#ifndef COEXISTENCE_802154_H
#define COEXISTENCE_802154_H

typedef enum {
  PTA_STACK_EVENT_RX_IDLED,
  PTA_STACK_EVENT_RX_LISTEN,
} halPtaStackEvent_t;

EmberStatus halPtaStackEvent(halPtaStackEvent_t event, uint32_t supplement);

#endif
