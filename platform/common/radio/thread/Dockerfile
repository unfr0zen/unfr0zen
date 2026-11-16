# The build image, as utils/build-firmware.sh tags it locally.
# hadolint ignore=DL3006
FROM unfr0zen-build
RUN apt-get update && apt-get install -y --no-install-recommends \
    cmake ninja-build libnewlib-arm-none-eabi libstdc++-arm-none-eabi-dev \
    && rm -rf /var/lib/apt/lists/*
