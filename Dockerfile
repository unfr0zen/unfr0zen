# The build image, for CI and for scripts/build-firmware.sh on macOS: the firmware
# toolchain, zig for nestlocal and the Linux and macOS app builds, MinGW for
# Windows, Apple's macOS SDK with rcodesign for the app bundle, and oras for the
# firmware CI keeps in its own registry. CI builds it
# on the runner and serves it from its own registry; build-firmware.sh builds it
# locally.

# The macOS SDK, from the Command Line Tools package on Apple's software-update
# CDN: a xar archive holding a pbzx-compressed cpio.
FROM debian:trixie-slim AS macos-sdk
ARG SDK_PKG=https://swcdn.apple.com/content/downloads/58/48/082-83364-A_KCEBOO2NJS/0d2rj4y5ucjlkgcvqt6f6a4pergug5tu2b/CLTools_macOSNMOS_SDK.pkg
ARG SDK_SHA256=d55351824fd17742fd6e1e7a252fa1028698f32147ee48be9a6b75442bf30575
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl 7zip cpio python3 \
    && curl -fsSL -o /sdk.pkg "$SDK_PKG" \
    && echo "$SDK_SHA256  /sdk.pkg" | sha256sum -c \
    && mkdir /x && cd /x && 7z x -txar /sdk.pkg Payload > /dev/null
RUN <<EOF python3
import lzma, struct
src, dst = open("/x/Payload", "rb"), open("/x/payload.cpio", "wb")
assert src.read(4) == b"pbzx"
src.read(8)
while header := src.read(16):
    chunk = src.read(struct.unpack(">QQ", header)[1])
    dst.write(lzma.decompress(chunk) if chunk[:6] == b"\xfd7zXZ\x00" else chunk)
EOF
RUN cd /x && cpio -idm --quiet < payload.cpio \
    && mv Library/Developer/CommandLineTools/SDKs/MacOSX*.*.sdk /MacOSX.sdk

FROM rust:1-trixie
RUN for arch in arm64 riscv64; do dpkg --add-architecture $arch; done \
    && apt-get update \
    && apt-get install -y --no-install-recommends nodejs git pkg-config bc bison flex libssl-dev \
        gcc-arm-none-eabi binutils-arm-none-eabi python3-pil python3-pip zstd zip \
        libudev-dev libudev-dev:arm64 libudev-dev:riscv64 gcc-mingw-w64-x86-64 llvm \
        clang cmake ninja-build libnewlib-arm-none-eabi libstdc++-arm-none-eabi-dev \
    && rm -rf /var/lib/apt/lists/* \
    && pip install --no-cache-dir --break-system-packages --root-user-action=ignore \
        ziglang==0.16.0 cargo-zigbuild==0.23.4 unicorn==2.1.4 pyelftools==0.33 \
    && rustup target add aarch64-unknown-linux-gnu x86_64-pc-windows-gnu \
        aarch64-apple-darwin x86_64-apple-darwin armv7-unknown-linux-musleabihf armv7-unknown-linux-gnueabihf \
        riscv64gc-unknown-linux-gnu aarch64-pc-windows-gnullvm \
    && printf '#!/bin/sh\nexec /usr/lib/llvm-19/bin/llvm-windres --target=aarch64-w64-mingw32 "$@"\n' \
        > /usr/local/bin/llvm-windres && chmod +x /usr/local/bin/llvm-windres \
    && mkdir /opt/windows-arm64 \
    && printf 'LIBRARY api-ms-win-core-synch-l1-2-0.dll\nEXPORTS\nWaitOnAddress\nWakeByAddressAll\nWakeByAddressSingle\n' \
        > /tmp/synchronization.def \
    && /usr/lib/llvm-19/bin/llvm-dlltool -m arm64 -d /tmp/synchronization.def -l /opt/windows-arm64/libsynchronization.a \
    && git config --system --add safe.directory '*' \
    && mkdir /opt/libudev && ln -s /usr/include/libudev.h /opt/libudev/ \
    && curl -fsSL -o /tmp/rcodesign.tar.gz https://github.com/indygreg/apple-platform-rs/releases/download/apple-codesign/0.29.0/apple-codesign-0.29.0-x86_64-unknown-linux-musl.tar.gz \
    && echo "dbe85cedd8ee4217b64e9a0e4c2aef92ab8bcaaa41f20bde99781ff02e600002  /tmp/rcodesign.tar.gz" | sha256sum -c \
    && tar xzf /tmp/rcodesign.tar.gz -C /usr/local/bin --strip-components=1 apple-codesign-0.29.0-x86_64-unknown-linux-musl/rcodesign \
    && rm /tmp/rcodesign.tar.gz \
    && curl -fsSL -o /tmp/oras.tar.gz https://github.com/oras-project/oras/releases/download/v1.3.4/oras_1.3.4_linux_amd64.tar.gz \
    && echo "f27adb935022d94df8dc77719c322dda592c78a0d57a6f7dcdd8d900b248c454  /tmp/oras.tar.gz" | sha256sum -c \
    && tar xzf /tmp/oras.tar.gz -C /usr/local/bin oras \
    && rm /tmp/oras.tar.gz
COPY --from=macos-sdk /MacOSX.sdk /opt/MacOSX.sdk
# zig compiles against its own glibc 2.28 headers; hidapi only needs libudev.h
# from the system, and libudev.so for each other Linux architecture comes from
# its multiarch package. For the arm64 Windows target (aarch64-pc-windows-gnullvm)
# winresource runs llvm-windres, which needs the target named, and zig's
# MinGW has no libsynchronization.a, so its import library is made here.
# zig targets macOS 13 at the oldest, and leaves no room for a code signature in
# x86_64 Mach-O binaries unless asked to pad the header (it signs arm64 itself).
ENV CFLAGS_x86_64_unknown_linux_gnu="-isystem /opt/libudev" \
    CFLAGS_aarch64_unknown_linux_gnu="-isystem /opt/libudev" \
    CARGO_TARGET_AARCH64_PC_WINDOWS_GNULLVM_RUSTFLAGS="-L native=/opt/windows-arm64" \
    CFLAGS_riscv64gc_unknown_linux_gnu="-isystem /opt/libudev" \
    PKG_CONFIG_ALLOW_CROSS=1 \
    PKG_CONFIG_LIBDIR_aarch64_unknown_linux_gnu=/usr/lib/aarch64-linux-gnu/pkgconfig:/usr/share/pkgconfig \
    PKG_CONFIG_LIBDIR_riscv64gc_unknown_linux_gnu=/usr/lib/riscv64-linux-gnu/pkgconfig:/usr/share/pkgconfig \
    SDKROOT=/opt/MacOSX.sdk \
    MACOSX_DEPLOYMENT_TARGET=13.0 \
    CARGO_TARGET_X86_64_APPLE_DARWIN_RUSTFLAGS="-C link-arg=-Wl,-headerpad,0x4000"
