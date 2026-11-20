#!/bin/sh
# Builds Stockroom as a macOS app in dist/macos, with a zip of it to hand out:
#
#   packaging/macos/build-app.sh                 Stockroom.app
#   packaging/macos/build-app.sh --for specular  "Install Specular.app", which opens on
#                                                Specular's install (README, "Handing it out
#                                                with an app")
#
# Universal (Apple silicon and Intel) when both Rust targets are installed. Signed ad hoc,
# or, with STOCKROOM_SIGN_IDENTITY set to a "Developer ID Application" certificate, with that
# and the hardened runtime. STOCKROOM_NOTARY_PROFILE (a `xcrun notarytool store-credentials`
# profile) also notarises the zip and staples the ticket to the app. --profile picks the
# cargo profile (default dist).
#
# Hand out the zip, not a disk image: a copy of Stockroom run from a mounted image or from where
# Gatekeeper translocates it keeps a copy of itself either way, but a zip opens with one
# double-click.
set -eu
cd "$(dirname "$0")/../.."

FOR=
PROFILE=dist
while [ $# -gt 0 ]; do
    case $1 in
    --for) FOR=${2:?--for needs an app ID}; shift 2 ;;
    --profile) PROFILE=${2:?--profile needs a name}; shift 2 ;;
    *) echo "usage: $0 [--for APP] [--profile PROFILE]" >&2; exit 2 ;;
    esac
done

ident() { sed -n "s/^pub const $1: &str = \"\(.*\)\";\$/\1/p" crates/suite-identity/src/lib.rs; }
NAME=$(ident NAME)
APP_ID=$(ident APP_ID)
SLUG=$(ident SLUG)
VERSION=$(sed -n 's/^version = "\(.*\)"$/\1/p' Cargo.toml | head -n 1)
MIN_OS=11.0

DISPLAY=$NAME
if [ -n "$FOR" ]; then
    META=catalog/apps/$FOR.toml
    [ -f "$META" ] || { echo "no app \"$FOR\" in catalog/apps" >&2; exit 2; }
    DISPLAY="Install $(sed -n '/^\[/q; s/^name = "\(.*\)"$/\1/p' "$META")"
fi

TARGETS=
for t in aarch64-apple-darwin x86_64-apple-darwin; do
    if rustup target list --installed | grep -qx "$t"; then TARGETS="$TARGETS $t"; fi
done
set -- $TARGETS
[ $# -gt 0 ] || { echo "no macOS Rust target is installed" >&2; exit 1; }
[ $# -eq 2 ] || echo "note: only$TARGETS is installed, so the app is not universal (rustup target add x86_64-apple-darwin aarch64-apple-darwin)" >&2

DIR=$PROFILE
[ "$PROFILE" = dev ] && DIR=debug
BINS=
for t in $TARGETS; do
    MACOSX_DEPLOYMENT_TARGET=$MIN_OS cargo build --locked --profile "$PROFILE" --target "$t" -p suite-cli
    BINS="$BINS target/$t/$DIR/$SLUG"
done

OUT=dist/macos
APP="$OUT/$DISPLAY.app"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
# shellcheck disable=SC2086
lipo -create -output "$APP/Contents/MacOS/$SLUG" $BINS
cp packaging/icons/stockroom.icns "$APP/Contents/Resources/$SLUG.icns"
sed -e "s|@DISPLAY_NAME@|$DISPLAY|g" -e "s|@EXECUTABLE@|$SLUG|g" -e "s|@APP_ID@|$APP_ID|g" \
    -e "s|@VERSION@|$VERSION|g" -e "s|@MIN_OS@|$MIN_OS|g" \
    packaging/macos/Info.plist.in >"$APP/Contents/Info.plist"
plutil -lint "$APP/Contents/Info.plist" >/dev/null
printf 'APPL????' >"$APP/Contents/PkgInfo"

if [ -n "${STOCKROOM_SIGN_IDENTITY:-}" ]; then
    codesign --force --options runtime --timestamp --sign "$STOCKROOM_SIGN_IDENTITY" "$APP"
else
    codesign --force --sign - "$APP"
fi
codesign --verify --strict "$APP"

ZIP="$OUT/$(printf '%s' "$DISPLAY" | tr ' ' '-')-$VERSION-macos.zip"
rm -f "$ZIP"
ditto -c -k --keepParent "$APP" "$ZIP"
if [ -n "${STOCKROOM_NOTARY_PROFILE:-}" ]; then
    [ -n "${STOCKROOM_SIGN_IDENTITY:-}" ] || { echo "notarising needs STOCKROOM_SIGN_IDENTITY" >&2; exit 1; }
    xcrun notarytool submit "$ZIP" --keychain-profile "$STOCKROOM_NOTARY_PROFILE" --wait
    xcrun stapler staple "$APP"
    rm -f "$ZIP"
    ditto -c -k --keepParent "$APP" "$ZIP"
fi
echo "$APP"
echo "$ZIP"
