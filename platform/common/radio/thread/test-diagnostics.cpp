#include <cassert>
#include <cstdarg>
#include <cstdio>
#include <string>
/* Native producer test: OpenThread APIs supply a public network plus secret keys. */
#include "host-cli.cpp"
static std::string output;
void otCliOutputFormat(const char *fmt, ...) {
    char line[4096]; va_list args; va_start(args, fmt);
    int n = vsnprintf(line, sizeof(line), fmt, args); va_end(args);
    if (n < 0 || static_cast<unsigned>(n) >= sizeof(line)) abort();
    output.append(line, n);
}
void otCliInit(otInstance *, otCliOutputCallback, void *) {}
otError otCliSetUserCommands(const otCliCommand *, uint8_t, void *) { return OT_ERROR_NONE; }
void otCliAppendResult(otError) {}
otError otThreadDiscover(otInstance *, uint32_t, uint16_t, bool, bool, otHandleActiveScanResult, void *) { return OT_ERROR_NONE; }
otDeviceRole otThreadGetDeviceRole(otInstance *) { return OT_DEVICE_ROLE_ROUTER; }
otLinkModeConfig otThreadGetLinkMode(otInstance *) { return {true, true, true}; }
bool otThreadIsRouterEligible(otInstance *) { return true; }
uint8_t otLinkGetChannel(otInstance *) { return 20; }
const otExtAddress *otLinkGetExtendedAddress(otInstance *) { static otExtAddress a = {{1,2,3,4,5,6,7,8}}; return &a; }
uint16_t otThreadGetRloc16(otInstance *) { return 0x5400; }
otError otThreadGetLeaderData(otInstance *, otLeaderData *l) { *l = {12345,64,8,7,21}; return OT_ERROR_NONE; }
otError otDatasetGetActive(otInstance *, otOperationalDataset *d) {
    memset(d, 0, sizeof(*d));
    d->mSecurityPolicy.mRotationTime = 672;
    d->mSecurityPolicy.mObtainNetworkKeyEnabled = true;
    d->mSecurityPolicy.mNativeCommissioningEnabled = true;
    d->mSecurityPolicy.mRoutersEnabled = true;
    d->mSecurityPolicy.mExternalCommissioningEnabled = true;
    d->mSecurityPolicy.mCommercialCommissioningEnabled = true;
    auto &c=d->mComponents;
    c.mIsActiveTimestampPresent=c.mIsNetworkKeyPresent=c.mIsNetworkNamePresent=c.mIsExtendedPanIdPresent=true;
    c.mIsMeshLocalPrefixPresent=c.mIsDelayPresent=c.mIsPanIdPresent=c.mIsChannelPresent=true;
    c.mIsPskcPresent=c.mIsSecurityPolicyPresent=c.mIsChannelMaskPresent=true;
    return OT_ERROR_NONE;
}
void otDatasetConvertToTlvs(const otOperationalDataset *, otOperationalDatasetTlvs *t) {
    const char *hex="00030000140102abcd020801020304050607080304746573740510000102030405060708090a0b0c0d0e0f04100f0e0d0c0b0a090807060504030201000708fd000000000000003506000407fff800";
    t->mLength = strlen(hex)/2;
    for(unsigned n=0;n<t->mLength;n++) { unsigned v; sscanf(hex+n*2,"%2x",&v);t->mTlvs[n]=v; }
}
otError otThreadGetNextNeighborInfo(otInstance *, otNeighborInfoIterator *i, otNeighborInfo *n) {
    if ((*i)++) return OT_ERROR_NOT_FOUND;
    memset(n,0,sizeof(*n)); n->mExtAddress={{0x10,0x20,0x30,0x40,0x50,0x60,0x70,0x80}};
    n->mAge=12; n->mRloc16=0x5401; n->mLinkFrameCounter=123; n->mMleFrameCounter=456;
    n->mLinkQualityIn=3; n->mAverageRssi=-65;n->mLastRssi=127;n->mFrameErrorRate=656;n->mMessageErrorRate=1311;n->mIsChild=true;
    return OT_ERROR_NONE;
}
otError otThreadGetRouterInfo(otInstance *, uint16_t id, otRouterInfo *r) {
    if (id!=22) return OT_ERROR_NOT_FOUND;
    memset(r,0,sizeof(*r));r->mExtAddress={{0x11,0x22,0x33,0x44,0x55,0x66,0x77,0x88}};
    r->mRloc16=0x5800;r->mRouterId=22;r->mNextHop=22;r->mPathCost=1;
    r->mLinkQualityIn=3;r->mLinkQualityOut=2;r->mAge=9;r->mAllocated=true;r->mLinkEstablished=true;
    return OT_ERROR_NONE;
}
const otMacCounters *otLinkGetCounters(otInstance *) {
    static otMacCounters m={};
    uint32_t *p=reinterpret_cast<uint32_t *>(&m);
    static_assert(sizeof(m)==34*sizeof(uint32_t),"counter layout");
    for(unsigned n=0;n<34;n++)p[n]=n;
    return &m;
}
int main() {
    if(diagnostics(nullptr,0,nullptr)!=OT_ERROR_NONE) return 1;
    assert(output.find("0510000102030405060708090a0b0c0d0e0f") == std::string::npos);
    assert(output.find("04100f0e0d0c0b0a09080706050403020100") == std::string::npos);
    assert(output.find("\"policy\":[672,31]") != std::string::npos);
    assert(output.find("\"role\":5,\"channel\":20") != std::string::npos);
    assert(output.find("\"1122334455667788\",22528,22,22,1,3,2,9,1,1") != std::string::npos);
    fputs(output.c_str(),stdout);
}
