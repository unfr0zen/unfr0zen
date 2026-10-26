/* OpenThread host suspend: the EM3581 wakes Linux for the next deadline. */
#include <openthread/openthread-system.h>
#include <sys/socket.h>
#include <sys/un.h>
#include <fcntl.h>
#include <errno.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <time.h>
#include <unistd.h>

/* CLOCK_BOOTTIME, which includes suspend; a kernel older than 2.6.39 (the
   gen2's) has none, and nestlocal then keeps the unit awake while Thread
   runs, so the monotonic clock serves. */
static clockid_t bootClock(void)
{
    static clockid_t clock = -1;
    struct timespec probe;
    if (clock == (clockid_t)-1) clock = clock_gettime(CLOCK_BOOTTIME, &probe) == 0 ? CLOCK_BOOTTIME : CLOCK_MONOTONIC;
    return clock;
}

extern "C" int nestThreadClock(clockid_t clock, struct timespec *now)
{
    if (clock == CLOCK_MONOTONIC_RAW || clock == CLOCK_MONOTONIC) clock = bootClock();
    return clock_gettime(clock, now);
}

static void fail(const char *operation)
{
    perror(operation);
    exit(EXIT_FAILURE);
}

extern "C" int nestThreadMainloopPoll(otSysMainloopContext *context)
{
    static int alarm = -1, state = -1;
    static uint32_t sequence = 0;
    static bool alarmDisabled = false;
    if (alarm < 0)
    {
        alarm = socket(AF_UNIX, SOCK_DGRAM | SOCK_CLOEXEC, 0);
        if (alarm < 0) fail("Thread alarm socket");
        struct sockaddr_un address = {};
        address.sun_family = AF_UNIX;
        snprintf(address.sun_path, sizeof(address.sun_path), "/run/nestlocal-thread-alarm.sock");
        unlink(address.sun_path);
        if (bind(alarm, (struct sockaddr *)&address, sizeof(address))) fail("Thread bind alarm socket");
        snprintf(address.sun_path, sizeof(address.sun_path), "/run/nestlocal-thread-wake.sock");
        if (connect(alarm, (struct sockaddr *)&address, sizeof(address))) fail("Thread connect wake bridge");
        struct timeval timeout = {2, 0};
        if (setsockopt(alarm, SOL_SOCKET, SO_RCVTIMEO, &timeout, sizeof(timeout))) fail("Thread alarm timeout");
        state = open("/run/nestlocal-thread-sleep", O_WRONLY | O_CREAT | O_TRUNC | O_CLOEXEC, 0600);
        if (state < 0) fail("Thread sleep state");
    }
    struct timespec now;
    if (clock_gettime(bootClock(), &now)) fail("Thread clock");
    uint64_t delay = (uint64_t)context->mTimeout.tv_sec * 1000000 + context->mTimeout.tv_usec;
    // Wake ahead of the deadline to cover IRQ delivery and CPU resume latency.
    struct { uint32_t sequence, milliseconds; } request = {++sequence, 0}, reply;
    if (delay > 300000) request.milliseconds = (uint32_t)((delay - 300000) / 1000);
    if (request.milliseconds != 0 || !alarmDisabled)
    {
        if (send(alarm, &request, sizeof(request), 0) != sizeof(request)) fail("Thread request wake alarm");
        // The bridge acknowledges only after the radio has accepted the alarm.
        for (;;)
        {
            ssize_t length = recv(alarm, &reply, sizeof(reply), 0);
            if (length < 0 && errno == EINTR) continue;
            if (length < 0) fail("Thread arm wake alarm");
            if (length == sizeof(reply) && reply.sequence == request.sequence) break;
        }
        alarmDisabled = request.milliseconds == 0;
    }
    struct { uint32_t pid, reserved; uint64_t deadline; } record = {(uint32_t)getpid(), 0, 0};
    // Short waits remain awake. The clock starts before the arm handshake.
    if (delay > 500000) record.deadline = (uint64_t)now.tv_sec * 1000000 + now.tv_nsec / 1000 + delay - 300000;
    if (pwrite(state, &record, sizeof(record), 0) != sizeof(record)) fail("Thread publish idle");
    FD_SET(alarm, &context->mReadFdSet);
    if (alarm > context->mMaxFd) context->mMaxFd = alarm;
    int result = otSysMainloopPoll(context);
    record.deadline = 0;
    if (pwrite(state, &record, sizeof(record), 0) != sizeof(record)) fail("Thread publish busy");
    // Empty datagrams signal a radio alarm; serial data arrives over the PTY.
    while (recv(alarm, &reply, sizeof(reply), MSG_DONTWAIT) >= 0) {}
    return result;
}
