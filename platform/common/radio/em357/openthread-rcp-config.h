#ifndef UNFR0ZEN_OPENTHREAD_RCP_CONFIG_EM357_H
#define UNFR0ZEN_OPENTHREAD_RCP_CONFIG_EM357_H

/* The EM3581's configuration, with the NCP's buffers sized for the EM357's
   12 KiB of RAM: a Spinel frame carries at most one 127-byte radio frame and
   its metadata, and frames to the host go out as they are made. */
#include "../em3581/openthread-rcp-config.h"

#define OPENTHREAD_CONFIG_NCP_TX_BUFFER_SIZE 768
#define OPENTHREAD_CONFIG_NCP_HDLC_TX_CHUNK_SIZE 256
#define OPENTHREAD_CONFIG_NCP_HDLC_RX_BUFFER_SIZE 320

#endif
