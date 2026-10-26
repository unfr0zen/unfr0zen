/* Thread discovery fields consumed by Matter's Network Commissioning cluster. */
#include <openthread/cli.h>
#include <openthread/thread.h>
#include <openthread/thread_ftd.h>
#include <openthread/dataset.h>
#include <string.h>
#include <stdlib.h>

static void hex(const uint8_t *data, unsigned length)
{
    for (unsigned i = 0; i < length; i++) otCliOutputFormat("%02x", data[i]);
}

static void discovered(otActiveScanResult *result, void *)
{
    if (!result) {
        otCliAppendResult(OT_ERROR_NONE);
        return;
    }
    otCliOutputFormat("NET ");
    hex(reinterpret_cast<const uint8_t *>(result->mNetworkName.m8), strlen(result->mNetworkName.m8));
    otCliOutputFormat(" ");
    hex(result->mExtendedPanId.m8, sizeof(result->mExtendedPanId.m8));
    otCliOutputFormat(" %04x ", result->mPanId);
    hex(result->mExtAddress.m8, sizeof(result->mExtAddress.m8));
    otCliOutputFormat(" %u %d %u %u\r\n", result->mChannel, result->mRssi, result->mLqi, result->mVersion);
}

static otError discover(void *context, uint8_t argc, char *[])
{
    if (argc) return OT_ERROR_INVALID_ARGS;
    otError error = otThreadDiscover(static_cast<otInstance *>(context), 0, OT_PANID_BROADCAST,
                                    false, false, discovered, nullptr);
    return error == OT_ERROR_NONE ? OT_ERROR_PENDING : error;
}

/* Public diagnostics only: dataset keys and PSKc never leave this command. */
static otError diagnostics(void *context, uint8_t argc, char *[])
{
    if (argc) return OT_ERROR_INVALID_ARGS;
    auto *instance = static_cast<otInstance *>(context);
    unsigned role = 0;
    switch (otThreadGetDeviceRole(instance)) {
    case OT_DEVICE_ROLE_DISABLED: role = 0; break;
    case OT_DEVICE_ROLE_DETACHED: role = 1; break;
    case OT_DEVICE_ROLE_CHILD: {
        auto mode = otThreadGetLinkMode(instance);
        role = !mode.mRxOnWhenIdle ? 2 : otThreadIsRouterEligible(instance) ? 4 : 3;
        break;
    }
    case OT_DEVICE_ROLE_ROUTER: role = 5; break;
    case OT_DEVICE_ROLE_LEADER: role = 6; break;
    }
    otCliOutputFormat("{\"role\":%u,\"channel\":%u,\"ext_address\":\"", role, otLinkGetChannel(instance));
    hex(otLinkGetExtendedAddress(instance)->m8, OT_EXT_ADDRESS_SIZE);
    otCliOutputFormat("\",\"rloc16\":%u,\"leader\":", otThreadGetRloc16(instance));
    otLeaderData leader;
    if (otThreadGetLeaderData(instance, &leader) == OT_ERROR_NONE)
        otCliOutputFormat("[%lu,%u,%u,%u,%u]", static_cast<unsigned long>(leader.mPartitionId),
                          leader.mWeighting, leader.mDataVersion, leader.mStableDataVersion, leader.mLeaderRouterId);
    else otCliOutputFormat("null");
    otOperationalDataset dataset;
    unsigned components = 0;
    bool have_dataset = otDatasetGetActive(instance, &dataset) == OT_ERROR_NONE;
    otCliOutputFormat(",\"policy\":");
    if (have_dataset && dataset.mComponents.mIsSecurityPolicyPresent) {
        const auto &p = dataset.mSecurityPolicy;
        unsigned flags = p.mObtainNetworkKeyEnabled | (p.mNativeCommissioningEnabled << 1) |
            (p.mRoutersEnabled << 2) | (p.mExternalCommissioningEnabled << 3) |
            (p.mCommercialCommissioningEnabled << 4) | (p.mAutonomousEnrollmentEnabled << 5) |
            (p.mNetworkKeyProvisioningEnabled << 6) | (p.mTobleLinkEnabled << 7) |
            (p.mNonCcmRoutersEnabled << 8) | (p.mVersionThresholdForRouting << 9);
        otCliOutputFormat("[%u,%u]", p.mRotationTime, flags);
    } else otCliOutputFormat("null");
    otCliOutputFormat(",\"dataset\":\"");
    if (have_dataset) {
        const auto &c = dataset.mComponents;
        components = c.mIsActiveTimestampPresent | (c.mIsPendingTimestampPresent << 1) |
            (c.mIsNetworkKeyPresent << 2) | (c.mIsNetworkNamePresent << 3) |
            (c.mIsExtendedPanIdPresent << 4) | (c.mIsMeshLocalPrefixPresent << 5) |
            (c.mIsDelayPresent << 6) | (c.mIsPanIdPresent << 7) | (c.mIsChannelPresent << 8) |
            (c.mIsPskcPresent << 9) | (c.mIsSecurityPolicyPresent << 10) | (c.mIsChannelMaskPresent << 11);
        otOperationalDatasetTlvs tlvs;
        otDatasetConvertToTlvs(&dataset, &tlvs);
        for (unsigned n = 0; n + 2 <= tlvs.mLength;) {
            unsigned type = tlvs.mTlvs[n], size = tlvs.mTlvs[n + 1];
            if (n + 2 + size > tlvs.mLength) return OT_ERROR_PARSE;
            if (type == OT_MESHCOP_TLV_CHANNEL || type == OT_MESHCOP_TLV_PANID ||
                type == OT_MESHCOP_TLV_EXTPANID || type == OT_MESHCOP_TLV_NETWORKNAME ||
                type == OT_MESHCOP_TLV_MESHLOCALPREFIX ||
                type == OT_MESHCOP_TLV_CHANNELMASK) hex(tlvs.mTlvs + n, size + 2);
            n += size + 2;
        }
    }
    otCliOutputFormat("\",\"components\":%u,\"neighbors\":[", components);
    otNeighborInfoIterator iterator = OT_NEIGHBOR_INFO_ITERATOR_INIT;
    otNeighborInfo neighbor;
    bool first = true;
    while (otThreadGetNextNeighborInfo(instance, &iterator, &neighbor) == OT_ERROR_NONE) {
        otCliOutputFormat("%s[\"", first ? "" : ","); first = false;
        hex(neighbor.mExtAddress.m8, OT_EXT_ADDRESS_SIZE);
        otCliOutputFormat("\",%lu,%u,%lu,%lu,%u,%d,%d,%lu,%lu,%u,%u,%u,%u]",
            static_cast<unsigned long>(neighbor.mAge), neighbor.mRloc16,
            static_cast<unsigned long>(neighbor.mLinkFrameCounter), static_cast<unsigned long>(neighbor.mMleFrameCounter),
            neighbor.mLinkQualityIn, neighbor.mAverageRssi, neighbor.mLastRssi,
            (neighbor.mFrameErrorRate * 100UL) / 65535, (neighbor.mMessageErrorRate * 100UL) / 65535,
            neighbor.mRxOnWhenIdle, neighbor.mFullThreadDevice, neighbor.mFullNetworkData, neighbor.mIsChild);
    }
    otCliOutputFormat("],\"routes\":[");
    first = true;
    for (uint8_t id = 0; id <= OT_NETWORK_MAX_ROUTER_ID; id++) {
        otRouterInfo router;
        if (otThreadGetRouterInfo(instance, id, &router) != OT_ERROR_NONE || !router.mAllocated) continue;
        otCliOutputFormat("%s[\"", first ? "" : ","); first = false;
        hex(router.mExtAddress.m8, OT_EXT_ADDRESS_SIZE);
        otCliOutputFormat("\",%u,%u,%u,%u,%u,%u,%u,%u,%u]", router.mRloc16, router.mRouterId,
            router.mNextHop, router.mPathCost, router.mLinkQualityIn, router.mLinkQualityOut,
            router.mAge, router.mAllocated, router.mLinkEstablished);
    }
    const auto &m = *otLinkGetCounters(instance);
    otCliOutputFormat("],\"mac\":[");
#define COUNT(field) otCliOutputFormat("%lu,", static_cast<unsigned long>(m.field))
    COUNT(mTxTotal); COUNT(mTxUnicast); COUNT(mTxBroadcast); COUNT(mTxAckRequested); COUNT(mTxAcked);
    COUNT(mTxNoAckRequested); COUNT(mTxData); COUNT(mTxDataPoll); COUNT(mTxBeacon); COUNT(mTxBeaconRequest);
    COUNT(mTxOther); COUNT(mTxRetry); COUNT(mTxDirectMaxRetryExpiry); COUNT(mTxIndirectMaxRetryExpiry);
    COUNT(mTxErrCca); COUNT(mTxErrAbort); COUNT(mTxErrBusyChannel); COUNT(mRxTotal); COUNT(mRxUnicast);
    COUNT(mRxBroadcast); COUNT(mRxData); COUNT(mRxDataPoll); COUNT(mRxBeacon); COUNT(mRxBeaconRequest);
    COUNT(mRxOther); COUNT(mRxAddressFiltered); COUNT(mRxDestAddrFiltered); COUNT(mRxDuplicated);
    COUNT(mRxErrNoFrame); COUNT(mRxErrUnknownNeighbor); COUNT(mRxErrInvalidSrcAddr); COUNT(mRxErrSec); COUNT(mRxErrFcs);
#undef COUNT
    otCliOutputFormat("%lu]}\r\n", static_cast<unsigned long>(m.mRxErrOther));
    return OT_ERROR_NONE;
}

extern "C" void nestThreadCliInit(otInstance *instance, otCliOutputCallback callback, void *context)
{
    otCliInit(instance, callback, context);
    static const otCliCommand commands[] = {{"nest-discover", discover}, {"nest-diag", diagnostics}};
    if (otCliSetUserCommands(commands, 2, instance) != OT_ERROR_NONE) abort();
}
