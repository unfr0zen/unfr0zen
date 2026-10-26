/* Source matching runs in the PHY's receive-address ISR, before automatic ACK. */
#include PLATFORM_HEADER
#include <openthread/platform/radio.h>
#include <string.h>

/* Matches the host's OpenThread maximum child count. */
#define SOURCE_MATCH_SIZE 64
static bool enabled;
static uint16_t short_entries[SOURCE_MATCH_SIZE];
static otExtAddress ext_entries[SOURCE_MATCH_SIZE];
static uint8_t short_count, ext_count;

/* The EM3581 PHY tests child status bits 0x730 after its short-address lookup.
 * Both outcomes use valid indices: 0xff makes the PHY set pending unconditionally.
 * Only the PHY object references these adapters; the Thread personality points
 * the stack's status table (emChildStatus) at this one, the Zigbee personality
 * forwards to the stack's own lookups. */
static const uint16_t child_status[] = { 0x10, 0 };
const uint16_t *threadRadioChildStatus = child_status;
extern bool radioZigbee;
extern uint8_t emberChildIndex(uint16_t address);
extern bool emLongIdDataPendingForDevice(const uint8_t *address);

uint8_t threadRadioChildIndex(uint16_t address) {
  if (radioZigbee) return emberChildIndex(address);
  if (!enabled) return 0;
  for (uint8_t n = 0; n < short_count; n++) {
    if (short_entries[n] == address) return 0;
  }
  return 1;
}

bool threadRadioLongIdPending(const uint8_t *address) {
  if (radioZigbee) return emLongIdDataPendingForDevice(address);
  if (!enabled) return true;
  for (uint8_t n = 0; n < ext_count; n++) {
    if (!memcmp(ext_entries[n].m8, address, sizeof(ext_entries[n].m8))) return true;
  }
  return false;
}

void otPlatRadioEnableSrcMatch(otInstance *i, bool value) {
  ATOMIC(enabled = value;);
}

otError otPlatRadioAddSrcMatchShortEntry(otInstance *i, otShortAddress address) {
  otError error = OT_ERROR_NONE;
  ATOMIC(
    uint8_t n;
    for (n = 0; n < short_count; n++) {
      if (short_entries[n] == address) break;
    }
    if (n == short_count) {
      if (short_count == SOURCE_MATCH_SIZE) error = OT_ERROR_NO_BUFS;
      else short_entries[short_count++] = address;
    }
  );
  return error;
}

otError otPlatRadioAddSrcMatchExtEntry(otInstance *i, const otExtAddress *address) {
  otError error = OT_ERROR_NONE;
  ATOMIC(
    uint8_t n;
    for (n = 0; n < ext_count; n++) {
      if (!memcmp(ext_entries[n].m8, address->m8, sizeof(address->m8))) break;
    }
    if (n == ext_count) {
      if (ext_count == SOURCE_MATCH_SIZE) error = OT_ERROR_NO_BUFS;
      else ext_entries[ext_count++] = *address;
    }
  );
  return error;
}

otError otPlatRadioClearSrcMatchShortEntry(otInstance *i, otShortAddress address) {
  otError error = OT_ERROR_NO_ADDRESS;
  ATOMIC(
    for (uint8_t n = 0; n < short_count; n++) {
      if (short_entries[n] == address) {
        short_entries[n] = short_entries[--short_count];
        error = OT_ERROR_NONE;
        break;
      }
    }
  );
  return error;
}

otError otPlatRadioClearSrcMatchExtEntry(otInstance *i, const otExtAddress *address) {
  otError error = OT_ERROR_NO_ADDRESS;
  ATOMIC(
    for (uint8_t n = 0; n < ext_count; n++) {
      if (!memcmp(ext_entries[n].m8, address->m8, sizeof(address->m8))) {
        ext_entries[n] = ext_entries[--ext_count];
        error = OT_ERROR_NONE;
        break;
      }
    }
  );
  return error;
}

void otPlatRadioClearSrcMatchShortEntries(otInstance *i) { ATOMIC(short_count = 0;); }
void otPlatRadioClearSrcMatchExtEntries(otInstance *i) { ATOMIC(ext_count = 0;); }
