#!/bin/sh
# Build the universal unfr0zen binary and wrap it in a macOS .app bundle,
# into dist/unfr0zen.app. Run on macOS.
#
#     sh scripts/make_macos_app.sh
#
# The firmware under build/ must exist first (scripts/build-firmware.sh). The app
# is self-contained: the binary carries the firmware, busybox and nestlocal.
#
# UNFR0ZEN_BINARY=path bundles an already-built universal binary instead; CI
# builds one on Linux and signs the bundle with rcodesign there.
set -eu

repo_dir=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$repo_dir"
version=$(git describe --tag --always --dirty)
export UNFR0ZEN_BUILD_VERSION="$version"
app="$repo_dir/dist/unfr0zen.app"
# The oldest macOS the zig-built CI binary supports; local builds match it.
min_macos=13.0

if [ -z "${UNFR0ZEN_BINARY:-}" ]; then
    echo "Building universal unfr0zen ${version}"
    export MACOSX_DEPLOYMENT_TARGET=$min_macos
    rustup target add aarch64-apple-darwin x86_64-apple-darwin >/dev/null
    cargo build --release -p unfr0zen-gui --target aarch64-apple-darwin
    cargo build --release -p unfr0zen-gui --target x86_64-apple-darwin
fi

rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"

if [ -n "${UNFR0ZEN_BINARY:-}" ]; then
    cp "$UNFR0ZEN_BINARY" "$app/Contents/MacOS/unfr0zen"
else
    lipo -create -output "$app/Contents/MacOS/unfr0zen" \
        target/aarch64-apple-darwin/release/unfr0zen \
        target/x86_64-apple-darwin/release/unfr0zen
fi
cp assets/icon/unfr0zen.icns "$app/Contents/Resources/unfr0zen.icns"
printf 'APPL????' > "$app/Contents/PkgInfo"

cat > "$app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key><string>unfr0zen</string>
    <key>CFBundleDisplayName</key><string>unfr0zen</string>
    <key>CFBundleIdentifier</key><string>dev.unfr0zen.gui</string>
    <key>CFBundleExecutable</key><string>unfr0zen</string>
    <key>CFBundleIconFile</key><string>unfr0zen</string>
    <key>CFBundlePackageType</key><string>APPL</string>
    <key>CFBundleShortVersionString</key><string>${version}</string>
    <key>LSMinimumSystemVersion</key><string>${min_macos}</string>
    <key>NSHighResolutionCapable</key><true/>
    <key>LSApplicationCategoryType</key><string>public.app-category.utilities</string>
    <key>NSPrincipalClass</key><string>NSApplication</string>
    <key>NSHumanReadableCopyright</key><string>unfr0zen</string>
</dict>
</plist>
PLIST

# Ad-hoc signature: Apple Silicon refuses to run arm64 code with none.
if command -v codesign >/dev/null 2>&1; then
    codesign --force --deep --sign - "$app" >/dev/null 2>&1 \
        && echo "Signed ad-hoc (not notarized)" \
        || echo "codesign failed; the app will need 'xattr -dr com.apple.quarantine' if quarantined"
elif command -v rcodesign >/dev/null 2>&1; then
    rcodesign sign "$app" >/dev/null
    echo "Signed ad-hoc with rcodesign (not notarized)"
else
    echo "neither codesign nor rcodesign found; the bundle is unsigned"
fi

echo "Built $app"
if command -v lipo >/dev/null 2>&1; then
    echo "Universal: $(lipo -archs "$app/Contents/MacOS/unfr0zen")"
fi
