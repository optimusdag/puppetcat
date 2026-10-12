#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# SPDX-FileCopyrightText: 2026 PuppetCat contributors

# Builds a local, installable PuppetCat.app and a .dmg for this Mac's own
# architecture. This is the macOS half of .github/workflows/build-app.yml,
# adapted to run on a developer's own machine: no cross-target, no
# deployment-target floor (the app only needs to run here), and no
# Developer ID - it signs ad-hoc unless one is found in the keychain, the
# same "opens with a Gatekeeper warning" experience a build from a fork
# gets in CI. With a Developer ID and notarization credentials (APPLE_ID,
# APPLE_APP_SPECIFIC_PASSWORD, APPLE_TEAM_ID, e.g. from
# ~/.config/gitlabboards/apple-signing.env) it also notarizes and staples.
# NOTARIZE=0 signs without notarizing.
set -euo pipefail

cd "$(dirname "$0")/.."

arch=$(uname -m)
case "$arch" in
  arm64) target=aarch64-apple-darwin ;;
  x86_64) target=x86_64-apple-darwin ;;
  *)
    echo "package-macos.sh: unsupported architecture $arch" >&2
    exit 1
    ;;
esac

version=$(sed -n 's/^version = "\(.*\)"/\1/p' engine/Cargo.toml | head -1)
echo "Packaging PuppetCat $version for macOS ($arch)"

if ! command -v brew >/dev/null; then
  echo "package-macos.sh: Homebrew is required (for ffmpeg and dylibbundler)" >&2
  exit 1
fi
if ! brew --prefix ffmpeg >/dev/null 2>&1; then
  echo "Installing ffmpeg..."
  brew install ffmpeg
fi
if ! command -v dylibbundler >/dev/null; then
  echo "Installing dylibbundler..."
  brew install dylibbundler
fi
export FFMPEG_DIR
FFMPEG_DIR=$(brew --prefix ffmpeg)

# `local` (thin LTO, 16 codegen units) by default: the fat-LTO `app` profile
# spends minutes on one core to squeeze out a few percent. PROFILE=app builds
# what CI ships.
profile=${PROFILE:-local}
echo "Building (profile: $profile)..."
(cd engine && cargo build --profile "$profile" -p puppetcat)

# The MCP server ships beside the window: the stand-alone server forwards to
# the window, so the two must come from one build. It uses the release
# profile, not app: app aborts on panic, and the server relies on unwinding
# to turn a panicking tool call into an error instead of dying.
echo "Building the MCP server (profile: release)..."
(cd engine && cargo build --release -p puppetcat-agent --bin puppetcat-agent-mcp)

binary="engine/target/$profile/puppetcat"
mcp_binary="engine/target/release/puppetcat-agent-mcp"
for built in "$binary" "$mcp_binary"; do
  if [ ! -f "$built" ]; then
    echo "package-macos.sh: expected $built after the build" >&2
    exit 1
  fi
done

app="PuppetCat.app"
rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources" "$app/Contents/Frameworks"
cp "$binary" "$app/Contents/MacOS/puppetcat"
cp "$mcp_binary" "$app/Contents/MacOS/puppetcat-agent-mcp"

# The icon set Apple wants: each size and its @2x twin.
iconset=$(mktemp -d)/PuppetCat.iconset
mkdir -p "$iconset"
icons=assets/icons
cp "$icons/puppetcat_logo_16.png"  "$iconset/icon_16x16.png"
cp "$icons/puppetcat_logo_32.png"  "$iconset/icon_16x16@2x.png"
cp "$icons/puppetcat_logo_32.png"  "$iconset/icon_32x32.png"
cp "$icons/puppetcat_logo_64.png"  "$iconset/icon_32x32@2x.png"
cp "$icons/puppetcat_logo_128.png" "$iconset/icon_128x128.png"
cp "$icons/puppetcat_logo_256.png" "$iconset/icon_128x128@2x.png"
cp "$icons/puppetcat_logo_256.png" "$iconset/icon_256x256.png"
cp "$icons/puppetcat_logo_512.png" "$iconset/icon_256x256@2x.png"
cp "$icons/puppetcat_logo_512.png" "$iconset/icon_512x512.png"
iconutil -c icns "$iconset" -o "$app/Contents/Resources/PuppetCat.icns"
rm -rf "$(dirname "$iconset")"

min_system=$(sw_vers -productVersion | cut -d. -f1-2)
cat > "$app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>PuppetCat</string>
  <key>CFBundleDisplayName</key><string>PuppetCat</string>
  <key>CFBundleIdentifier</key><string>app.puppetcat.editor</string>
  <key>CFBundleVersion</key><string>${version}</string>
  <key>CFBundleShortVersionString</key><string>${version}</string>
  <key>CFBundleExecutable</key><string>puppetcat</string>
  <key>CFBundleIconFile</key><string>PuppetCat</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>LSMinimumSystemVersion</key><string>${min_system}</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>NSSupportsAutomaticGraphicsSwitching</key><true/>
  <key>LSApplicationCategoryType</key><string>public.app-category.video</string>
  <key>NSHumanReadableCopyright</key><string>AGPL-3.0-or-later. See THIRD_PARTY_NOTICES.md.</string>
</dict>
</plist>
PLIST

echo "Bundling dylibs..."
dylibbundler -od -b \
  -x "$app/Contents/MacOS/puppetcat" \
  -x "$app/Contents/MacOS/puppetcat-agent-mcp" \
  -d "$app/Contents/Frameworks" \
  -p @executable_path/../Frameworks

for bundled in puppetcat puppetcat-agent-mcp; do
  if otool -L "$app/Contents/MacOS/$bundled" | grep -E '/opt/homebrew|/usr/local'; then
    echo "package-macos.sh: $bundled still loads libraries from outside the bundle" >&2
    exit 1
  fi
done

identity=$(security find-identity -v -p codesigning 2>/dev/null \
  | sed -n 's/.*"\(Developer ID Application:[^"]*\)".*/\1/p' | head -1)
if [ -n "$identity" ]; then
  echo "Signing with $identity"
  find "$app/Contents/Frameworks" -name '*.dylib' -exec \
    codesign --force --options runtime --timestamp -s "$identity" {} \;
  codesign --force --options runtime --timestamp -s "$identity" "$app/Contents/MacOS/puppetcat-agent-mcp"
  codesign --force --options runtime --timestamp -s "$identity" "$app/Contents/MacOS/puppetcat"
  codesign --force --options runtime --timestamp -s "$identity" "$app"
  codesign --verify --deep --strict "$app"

  if [ "${NOTARIZE:-1}" != 0 ]; then
    creds=${APPLE_SIGNING_ENV:-$HOME/.config/gitlabboards/apple-signing.env}
    # shellcheck disable=SC1090
    [ -f "$creds" ] && . "$creds"
    for var in APPLE_ID APPLE_APP_SPECIFIC_PASSWORD APPLE_TEAM_ID; do
      if [ -z "${!var:-}" ]; then
        echo "package-macos.sh: $var is not set (see $creds); set NOTARIZE=0 to skip notarization" >&2
        exit 1
      fi
    done
    echo "Notarizing (this waits on Apple, usually a few minutes)..."
    zip=$(mktemp -d)/PuppetCat.zip
    ditto -c -k --keepParent "$app" "$zip"
    xcrun notarytool submit "$zip" --apple-id "$APPLE_ID" \
      --password "$APPLE_APP_SPECIFIC_PASSWORD" --team-id "$APPLE_TEAM_ID" --wait
    rm -rf "$(dirname "$zip")"
    xcrun stapler staple "$app"
    xcrun stapler validate "$app"
    spctl -a -vv -t exec "$app"
  fi
else
  echo "No Developer ID in the keychain: signing ad-hoc (Gatekeeper will warn once on first open)."
  codesign --force --deep -s - "$app"
fi

dmg="PuppetCat-${version}-macos-${arch}.dmg"
rm -f "$dmg"
hdiutil create -volname PuppetCat -srcfolder "$app" -ov -format UDZO "$dmg"

echo
echo "Built $app and $dmg"
echo "MCP server: $PWD/$app/Contents/MacOS/puppetcat-agent-mcp"
