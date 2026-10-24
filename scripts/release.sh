#!/usr/bin/env bash
# Build, sign and publish a macOS release of Hangar.
#
#   scripts/release.sh all       bundle + sign + dmg (no tag, nothing pushed)
#   scripts/release.sh bundle    target/release/bundle/Hangar.app
#   scripts/release.sh sign      codesign the .app
#   scripts/release.sh dmg       target/release/Hangar-<version>-<arch>.dmg, signed
#   scripts/release.sh publish   tag v<version>, push it, create the GitHub release with the DMG
#
# Release notes come from docs/releases/v<version>.md; the requirements, install steps and SHA-256
# are appended here.
#
# HANGAR_SIGN_IDENTITY overrides the codesign identity (default: first "Apple Development"
# identity in the keychain, else ad-hoc "-"). Not notarized: needs a Developer ID certificate.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

VERSION="$(sed -n '/^\[workspace.package\]/,/^\[/s/^version = "\(.*\)"/\1/p' Cargo.toml)"
[[ -n "$VERSION" ]] || { echo "cannot read [workspace.package] version" >&2; exit 1; }
ARCH="$(uname -m)"
TAG="v$VERSION"
OUT="$ROOT/target/release"
APP="$OUT/bundle/Hangar.app"
DMG="$OUT/Hangar-$VERSION-$ARCH.dmg"
ICON_SRC="$ROOT/crates/app/assets/brand/hangar-app-icon-1024.png"
NOTES="$ROOT/docs/releases/$TAG.md"

log() { printf '\033[1m==> %s\033[0m\n' "$*"; }

identity() {
    if [[ -n "${HANGAR_SIGN_IDENTITY:-}" ]]; then
        echo "$HANGAR_SIGN_IDENTITY"
        return
    fi
    local id
    id="$(security find-identity -v -p codesigning | sed -n 's/.*\([0-9A-F]\{40\}\) "Apple Development.*/\1/p' | head -n1)"
    echo "${id:--}"
}

make_icns() {
    local iconset="$1/Hangar.iconset"
    mkdir -p "$iconset"
    for size in 16 32 128 256 512; do
        sips -z "$size" "$size" "$ICON_SRC" --out "$iconset/icon_${size}x${size}.png" >/dev/null
        sips -z $((size * 2)) $((size * 2)) "$ICON_SRC" --out "$iconset/icon_${size}x${size}@2x.png" >/dev/null
    done
    iconutil -c icns "$iconset" -o "$2"
}

cmd_bundle() {
    log "cargo build --release ($VERSION, $ARCH)"
    cargo build --release -p hangar-app
    log "assembling $APP"
    rm -rf "$APP"
    mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
    cp "$OUT/Hangar" "$APP/Contents/MacOS/Hangar"
    sed "s/@VERSION@/$VERSION/g" "$ROOT/crates/app/bundle/Info.plist" >"$APP/Contents/Info.plist"
    plutil -lint "$APP/Contents/Info.plist" >/dev/null
    local tmp
    tmp="$(mktemp -d)"
    make_icns "$tmp" "$APP/Contents/Resources/Hangar.icns"
    rm -rf "$tmp"
}

cmd_sign() {
    [[ -d "$APP" ]] || { echo "no $APP; run bundle first" >&2; exit 1; }
    local id
    id="$(identity)"
    log "codesign with $id"
    if [[ "$id" == "-" ]]; then
        codesign --force --deep -s - "$APP"
    else
        codesign --force --deep --timestamp -s "$id" "$APP"
    fi
    codesign --verify --deep --strict --verbose=2 "$APP"
}

cmd_dmg() {
    [[ -d "$APP" ]] || { echo "no $APP; run bundle first" >&2; exit 1; }
    log "building $DMG"
    local stage
    stage="$(mktemp -d)"
    cp -R "$APP" "$stage/"
    ln -s /Applications "$stage/Applications"
    rm -f "$DMG"
    hdiutil create -volname "Hangar $VERSION" -srcfolder "$stage" -ov -format UDZO "$DMG" >/dev/null
    rm -rf "$stage"
    local id
    id="$(identity)"
    if [[ "$id" == "-" ]]; then
        codesign --force -s - "$DMG"
    else
        codesign --force --timestamp -s "$id" "$DMG"
    fi
    shasum -a 256 "$DMG" | tee "$DMG.sha256"
}

release_notes() {
    cat "$NOTES"
    cat <<EOF

## Requirements
- Apple Silicon Mac ($ARCH build), macOS 14 or later.
- Git; Xcode for iOS; Android SDK for Android; the agents you want to run (see Settings › Setup).

## Install
Open the DMG and drag **Hangar** to Applications. This build is signed with a development
certificate but **not notarized**, so macOS blocks the first launch. Either right-click Hangar ›
Open › Open, or run:

\`\`\`
xattr -dr com.apple.quarantine /Applications/Hangar.app
\`\`\`

SHA-256: \`$(cut -d' ' -f1 "$DMG.sha256")\`
EOF
}

cmd_publish() {
    [[ -f "$DMG" && -f "$DMG.sha256" ]] || { echo "no $DMG; run all first" >&2; exit 1; }
    [[ -f "$NOTES" ]] || { echo "no release notes at $NOTES" >&2; exit 1; }
    [[ "$(git rev-parse --abbrev-ref HEAD)" == "main" ]] || { echo "publish from main" >&2; exit 1; }
    [[ -z "$(git status --porcelain)" ]] || { echo "working tree not clean" >&2; exit 1; }
    if git rev-parse -q --verify "refs/tags/$TAG" >/dev/null; then
        echo "tag $TAG already exists" >&2
        exit 1
    fi
    local notes
    notes="$(mktemp)"
    release_notes >"$notes"
    log "tagging $TAG"
    git tag -a "$TAG" -m "Hangar $VERSION"
    git push origin "$TAG"
    log "creating GitHub release $TAG"
    gh release create "$TAG" "$DMG" "$DMG.sha256" --title "Hangar $VERSION" --notes-file "$notes"
    rm -f "$notes"
}

case "${1:-}" in
    bundle) cmd_bundle ;;
    sign) cmd_sign ;;
    dmg) cmd_dmg ;;
    all)
        cmd_bundle
        cmd_sign
        cmd_dmg
        log "done: $DMG (smoke-test it, then: scripts/release.sh publish)"
        ;;
    publish) cmd_publish ;;
    *)
        sed -n '2,14p' "$0"
        exit 1
        ;;
esac
