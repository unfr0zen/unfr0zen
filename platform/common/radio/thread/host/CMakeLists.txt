cmake_minimum_required(VERSION 3.18)
project(unfr0zen-thread-host LANGUAGES C CXX)
add_subdirectory("${OT_SOURCE}" openthread)
target_sources(ot-daemon PRIVATE ../host-sleep.cpp ../host-cli.cpp)
# Room for main.c's own user commands beside the nest-* set.
target_compile_definitions(ot-config INTERFACE OPENTHREAD_CONFIG_CLI_MAX_USER_CMD_ENTRIES=2)
set_property(SOURCE "${OT_SOURCE}/src/posix/main.c" TARGET_DIRECTORY ot-daemon
    APPEND PROPERTY COMPILE_DEFINITIONS otSysMainloopPoll=nestThreadMainloopPoll)
# The daemon build initializes the CLI from the platform's system.cpp, not main.c.
set_property(SOURCE "${OT_SOURCE}/src/posix/platform/system.cpp" TARGET_DIRECTORY openthread-posix
    APPEND PROPERTY COMPILE_DEFINITIONS otCliInit=nestThreadCliInit)
set_property(SOURCE "${OT_SOURCE}/src/posix/platform/alarm.cpp" TARGET_DIRECTORY openthread-posix
    APPEND PROPERTY COMPILE_DEFINITIONS clock_gettime=nestThreadClock)
