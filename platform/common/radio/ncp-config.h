// The NCP's stack configuration: what AppBuilder's ncp-spi sample sets. The
// host sizes most tables at run time through EZSP configuration values. Not
// config.h: the stack's own config.h, beside its defaults header, would win.

#ifndef NCP_CONFIG_H
#define NCP_CONFIG_H

// ZigBee PRO. AppBuilder writes this into every Zigbee app's header; the
// stack's default is 0, and then it takes no ZigBee PRO beacon for a parent.
#define EMBER_STACK_PROFILE 2

// Room for the bindings a coordinator makes (one per reported cluster); the
// host sets how many are used, up to this.
#define EMBER_BINDING_TABLE_TOKEN_SIZE 16

#define EZSP_TOKEN_SIZE    8
#define EZSP_TOKEN_ENTRIES 32

#define SOURCE_ROUTE_TABLE_MEMORY 40
#define BINDING_TABLE_MEMORY      60
#define BROADCAST_TABLE_MEMORY    (EMBER_BROADCAST_TABLE_SIZE * 5)
#define EXTRA_MEMORY              (SOURCE_ROUTE_TABLE_MEMORY + BINDING_TABLE_MEMORY + BROADCAST_TABLE_MEMORY)
#define RESERVED_AVAILABLE_MEMORY (EMBER_PACKET_BUFFER_COUNT + EXTRA_MEMORY)

#endif
