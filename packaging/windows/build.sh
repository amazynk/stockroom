#!/bin/sh
# Builds Stockroom for Windows into dist/windows:
#
#   packaging/windows/build.sh                 Stockroom\ (stockroom.exe, stockroom.com, stockroom-shim.exe)
#                                              and a zip of it
#   packaging/windows/build.sh --for specular  also "Install Specular.exe", one file that opens
#                                              on Specular's install (README, "Handing it out
#                                              with an app")
#
# stockroom.exe is a windowed program with its icon, version and manifest built in (required
# here), and it carries the shim and stockroom.com inside itself, so a single downloaded .exe
# leaves a complete Stockroom behind (crates/suite-cli/build.rs). Built with MSVC on Windows, or
# from macOS and Linux for x86_64-pc-windows-gnu with MinGW; --target picks another.
# --profile picks the cargo profile (default dist). Not code-signed yet.
set -eu
cd "$(dirname "$0")/../.."

FOR=
PROFILE=dist
TARGET=
while [ $# -gt 0 ]; do
    case $1 in
    --for) FOR=${2:?--for needs an app ID}; shift 2 ;;
    --profile) PROFILE=${2:?--profile needs a name}; shift 2 ;;
    --target) TARGET=${2:?--target needs a triple}; shift 2 ;;
    *) echo "usage: $0 [--for APP] [--profile PROFILE] [--target TRIPLE]" >&2; exit 2 ;;
    esac
done
if [ -z "$TARGET" ]; then
    case $(uname -s) in
    MINGW* | MSYS* | CYGWIN*) TARGET=x86_64-pc-windows-msvc ;;
    *) TARGET=x86_64-pc-windows-gnu ;;
    esac
fi

ident() { sed -n "s/^pub const $1: &str = \"\(.*\)\";\$/\1/p" crates/suite-identity/src/lib.rs; }
NAME=$(ident NAME)
SLUG=$(ident SLUG)
VERSION=$(sed -n 's/^version = "\(.*\)"$/\1/p' Cargo.toml | head -n 1)
APP_NAME=
if [ -n "$FOR" ]; then
    META=catalog/apps/$FOR.toml
    [ -f "$META" ] || { echo "no app \"$FOR\" in catalog/apps" >&2; exit 2; }
    APP_NAME=$(sed -n '/^\[/q; s/^name = "\(.*\)"$/\1/p' "$META")
fi

DIR=$PROFILE
[ "$PROFILE" = dev ] && DIR=debug
BUILT=target/$TARGET/$DIR
cargo build --locked --profile "$PROFILE" --target "$TARGET" -p suite-shim -p suite-console
STOCKROOM_CARRY_DIR=$PWD/$BUILT STOCKROOM_WINDOWS_RESOURCES=required \
    cargo build --locked --profile "$PROFILE" --target "$TARGET" -p suite-cli

OUT=dist/windows
rm -rf "${OUT:?}/$NAME"
mkdir -p "$OUT/$NAME"
cp "$BUILT/$SLUG.exe" "$OUT/$NAME/$SLUG.exe"
cp "$BUILT/suite-console.exe" "$OUT/$NAME/$SLUG.com"
cp "$BUILT/suite-shim.exe" "$OUT/$NAME/$SLUG-shim.exe"
ZIP=$NAME-$VERSION-windows-x86_64.zip
if command -v zip >/dev/null; then
    (cd "$OUT" && rm -f "$ZIP" && zip -qr "$ZIP" "$NAME")
    echo "$OUT/$ZIP"
else
    echo "note: no zip command; $OUT/$NAME is not zipped" >&2
fi
echo "$OUT/$NAME"
if [ -n "$FOR" ]; then
    cp "$BUILT/$SLUG.exe" "$OUT/Install $APP_NAME.exe"
    echo "$OUT/Install $APP_NAME.exe"
fi
