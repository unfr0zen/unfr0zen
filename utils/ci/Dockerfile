# The build image, for CI and for utils/build-firmware.sh on macOS: the firmware
# toolchain, zig for nestlocal and the Linux and macOS app builds, MinGW for
# Windows, Apple's macOS SDK with rcodesign for the app bundle, nextest and pytest
# for the tests' JUnit reports, and oras, for amd64 and arm64 hosts. CI builds it
# for each and serves it from Docker Hub; build-firmware.sh builds it locally.

# The macOS SDK, from the Command Line Tools package on Apple's software-update
# CDN: a xar archive holding a pbzx-compressed cpio. Apple drops a package from
# the CDN once a newer one ships; the current one is the CLTools_macOSNMOS_SDK
# entry of the catalog at swscan.apple.com/content/catalogs/others/.
FROM debian:trixie-slim AS macos-sdk
ARG SDK_PKG=https://swcdn.apple.com/content/downloads/33/19/140-17812-A_21ZLMMLY4E/zu3xwktttpoe71qiawhzgzvqss6rovawsa/CLTools_macOSNMOS_SDK.pkg
ARG SDK_SHA256=debe353b27a13cd678b26ab216a040acf07b62c0639d8ac522df4ca07d121eb2
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
ARG TARGETARCH
RUN for arch in amd64 arm64 riscv64; do [ "$arch" = "$TARGETARCH" ] || dpkg --add-architecture $arch; done \
    && apt-get update \
    && apt-get install -y --no-install-recommends nodejs git pkg-config bc bison flex libssl-dev \
        gcc-arm-none-eabi binutils-arm-none-eabi python3-pil python3-pip python3-pytest zstd zip \
        libudev-dev:amd64 libudev-dev:arm64 libudev-dev:riscv64 gcc-mingw-w64-x86-64 llvm \
        clang cmake ninja-build libnewlib-arm-none-eabi libstdc++-arm-none-eabi-dev \
    && rm -rf /var/lib/apt/lists/* \
    && pip install --no-cache-dir --break-system-packages --root-user-action=ignore \
        ziglang==0.16.0 cargo-zigbuild==0.23.4 unicorn==2.1.4 pyelftools==0.33 \
    && rustup target add x86_64-unknown-linux-gnu aarch64-unknown-linux-gnu x86_64-pc-windows-gnu \
        aarch64-apple-darwin x86_64-apple-darwin armv7-unknown-linux-musleabihf \
        riscv64gc-unknown-linux-gnu aarch64-pc-windows-gnullvm \
    && printf '#!/bin/sh\nexec /usr/lib/llvm-19/bin/llvm-windres --target=aarch64-w64-mingw32 "$@"\n' \
        > /usr/local/bin/llvm-windres && chmod +x /usr/local/bin/llvm-windres \
    && mkdir /opt/windows-arm64 \
    && printf 'LIBRARY api-ms-win-core-synch-l1-2-0.dll\nEXPORTS\nWaitOnAddress\nWakeByAddressAll\nWakeByAddressSingle\n' \
        > /tmp/synchronization.def \
    && /usr/lib/llvm-19/bin/llvm-dlltool -m arm64 -d /tmp/synchronization.def -l /opt/windows-arm64/libsynchronization.a \
    && git config --system --add safe.directory '*' \
    && mkdir /opt/libudev && ln -s /usr/include/libudev.h /opt/libudev/ \
    && case "$TARGETARCH" in \
         amd64) rust_arch=x86_64 \
                rcodesign_sha=dbe85cedd8ee4217b64e9a0e4c2aef92ab8bcaaa41f20bde99781ff02e600002 \
                oras_sha=f27adb935022d94df8dc77719c322dda592c78a0d57a6f7dcdd8d900b248c454 \
                nextest_sha=1d4ce302b820c41f4b89c5fb9bce45f0f0c94882c1cf08fc360caf08be6e7df4 ;; \
         arm64) rust_arch=aarch64 \
                rcodesign_sha=4af92c87ddf52f5f2d1258a3b4e56c7dcb8f1b2468df744976c5f139e031961f \
                oras_sha=15702c6e3a4a56a8bd8ac5c17efdbcab56d9bada661ccbcf017f5b10c1d89399 \
                nextest_sha=3e35e8f6913af92558ba09d7155f74e35dd1f0024ef3cd891637433ae4eacbdd ;; \
         *) echo "unsupported TARGETARCH: $TARGETARCH" >&2; exit 1 ;; \
       esac \
    && rcodesign=apple-codesign-0.29.0-$rust_arch-unknown-linux-musl \
    && curl -fsSL -o /tmp/rcodesign.tar.gz "https://github.com/indygreg/apple-platform-rs/releases/download/apple-codesign/0.29.0/$rcodesign.tar.gz" \
    && echo "$rcodesign_sha  /tmp/rcodesign.tar.gz" | sha256sum -c \
    && tar xzf /tmp/rcodesign.tar.gz -C /usr/local/bin --strip-components=1 "$rcodesign/rcodesign" \
    && rm /tmp/rcodesign.tar.gz \
    && curl -fsSL -o /tmp/oras.tar.gz "https://github.com/oras-project/oras/releases/download/v1.3.4/oras_1.3.4_linux_$TARGETARCH.tar.gz" \
    && echo "$oras_sha  /tmp/oras.tar.gz" | sha256sum -c \
    && tar xzf /tmp/oras.tar.gz -C /usr/local/bin oras \
    && rm /tmp/oras.tar.gz \
    && curl -fsSL -o /tmp/nextest.tar.gz "https://github.com/nextest-rs/nextest/releases/download/cargo-nextest-0.9.151/cargo-nextest-0.9.151-$rust_arch-unknown-linux-gnu.tar.gz" \
    && echo "$nextest_sha  /tmp/nextest.tar.gz" | sha256sum -c \
    && tar xzf /tmp/nextest.tar.gz -C /usr/local/bin cargo-nextest \
    && rm /tmp/nextest.tar.gz
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
    PKG_CONFIG_LIBDIR_x86_64_unknown_linux_gnu=/usr/lib/x86_64-linux-gnu/pkgconfig:/usr/share/pkgconfig \
    PKG_CONFIG_LIBDIR_aarch64_unknown_linux_gnu=/usr/lib/aarch64-linux-gnu/pkgconfig:/usr/share/pkgconfig \
    PKG_CONFIG_LIBDIR_riscv64gc_unknown_linux_gnu=/usr/lib/riscv64-linux-gnu/pkgconfig:/usr/share/pkgconfig \
    SDKROOT=/opt/MacOSX.sdk \
    MACOSX_DEPLOYMENT_TARGET=13.0 \
    CARGO_TARGET_X86_64_APPLE_DARWIN_RUSTFLAGS="-C link-arg=-Wl,-headerpad,0x4000"
