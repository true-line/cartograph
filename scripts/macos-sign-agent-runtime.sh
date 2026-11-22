#!/usr/bin/env bash
# ADR-0085, AC-1427: re-signs the bundled cartograph-agent sidecar with its own
# entitlements (allow-jit, which V8 needs under the hardened runtime), then
# re-seals the app around it, so the app itself carries no entitlements.
#
# Tauri signs every external binary with the app's single entitlements file,
# so this runs after `tauri bundle` and before notarization:
#   1. sign the sidecar with src-tauri/agent-runtime.entitlements.plist;
#   2. sign the app, without --deep, keeping its own (empty) entitlements;
#   3. put that exact app into the disk image and sign the image;
#   4. check the entitlements of the sidecar and the app in both places, and
#      that Claude Code (AC-1424) still carries Anthropic's own signature.
#
# Usage: macos-sign-agent-runtime.sh <signed|unsigned-dev> <Cartograph.app> <image.dmg>
# Signed mode reads APPLE_CERTIFICATE (base64 .p12) and
# APPLE_CERTIFICATE_PASSWORD, imports them into a temporary keychain that is
# deleted on exit, and signs with the same team as Tauri's signature.
set -euo pipefail

mode=${1:?mode}
app=${2:?app}
dmg=${3:?dmg}
root=$(cd "$(dirname "$0")/.." && pwd)
entitlements="$root/src-tauri/agent-runtime.entitlements.plist"
jit='com.apple.security.cs.allow-jit'
agent="$app/Contents/MacOS/cartograph-agent"
claude_team=$(node -p 'JSON.parse(require("fs").readFileSync(process.argv[1], "utf8")).team_id' \
  "$root/scripts/agent-runtime-claude.json")
# The same requirement the build and the app check (Developer ID, that team).
claude_requirement="anchor apple generic and certificate 1[field.1.2.840.113635.100.6.2.6] exists and certificate leaf[field.1.2.840.113635.100.6.1.13] exists and certificate leaf[subject.OU] = \"$claude_team\""
test -f "$entitlements"
test -x "$agent"
test -f "$dmg"

work=$(mktemp -d "${RUNNER_TEMP:-/tmp}/cartograph-sign.XXXXXX")
keychain=''
searchlist=()
mounted=''
cleanup() {
  if [ -n "$mounted" ]; then hdiutil detach "$mounted" -force -quiet || true; fi
  if [ -n "$keychain" ]; then
    if [ "${#searchlist[@]}" -gt 0 ]; then security list-keychains -d user -s ${searchlist[@]+"${searchlist[@]}"} || true; fi
    security delete-keychain "$keychain" || true
  fi
  rm -rf "$work"
}
trap cleanup EXIT

team_of() {
  codesign -dvv "$1" 2>&1 | sed -n 's/^TeamIdentifier=//p'
}

case "$mode" in
  signed)
    : "${APPLE_CERTIFICATE:?APPLE_CERTIFICATE is required to sign}"
    : "${APPLE_CERTIFICATE_PASSWORD:?APPLE_CERTIFICATE_PASSWORD is required to sign}"
    while IFS= read -r line; do
      line=${line#"${line%%[![:space:]]*}"}
      line=${line#\"}
      searchlist+=("${line%\"}")
    done < <(security list-keychains -d user)
    keychain="$work/signing.keychain-db"
    password=$(openssl rand -hex 24)
    security create-keychain -p "$password" "$keychain"
    security set-keychain-settings -lut 1800 "$keychain"
    security unlock-keychain -p "$password" "$keychain"
    printf '%s' "$APPLE_CERTIFICATE" | base64 -D > "$work/certificate.p12"
    security import "$work/certificate.p12" -k "$keychain" -f pkcs12 \
      -P "$APPLE_CERTIFICATE_PASSWORD" -T /usr/bin/codesign >/dev/null
    rm -f "$work/certificate.p12"
    security set-key-partition-list -S apple-tool:,apple:,codesign: -s -k "$password" "$keychain" >/dev/null
    security list-keychains -d user -s "$keychain" ${searchlist[@]+"${searchlist[@]}"}
    identity=$(security find-identity -v -p codesigning "$keychain" \
      | awk -F'"' '/Developer ID Application/ { print $2; exit }')
    if [ -z "$identity" ]; then
      echo 'no Developer ID Application identity in the signing certificate' >&2
      exit 1
    fi
    team=$(team_of "$app")
    sign=(codesign --force --options runtime --timestamp --keychain "$keychain" --sign "$identity")
    ;;
  unsigned-dev)
    team=''
    sign=(codesign --force --options runtime --timestamp=none --sign -)
    ;;
  *)
    echo "unknown signing mode $mode" >&2
    exit 1
    ;;
esac

# 1-2. Inside out: the sidecar first, then the app's own seal. No --deep, so
# nothing else in the bundle is re-signed and no entitlements spread.
"${sign[@]}" --entitlements "$entitlements" "$agent"
"${sign[@]}" --preserve-metadata=entitlements,requirements "$app"
codesign --verify --strict --verbose=2 "$agent"
codesign --verify --deep --strict --verbose=2 "$app"
if [ "$mode" = signed ] && [ "$(team_of "$app")" != "$team" ]; then
  echo "re-signed app team $(team_of "$app") differs from Tauri's $team" >&2
  exit 1
fi

# 3. The disk image carries its own copy of the app: replace it with the one
# just signed, keeping the image's layout, then sign the image.
hdiutil convert "$dmg" -format UDSP -o "$work/image.sparseimage" -quiet
hdiutil resize -size 4g "$work/image.sparseimage" -quiet
mkdir "$work/mount"
hdiutil attach "$work/image.sparseimage" -nobrowse -noautoopen -mountpoint "$work/mount" -quiet
mounted="$work/mount"
name=$(basename "$app")
test -d "$work/mount/$name"
rm -rf "${work:?}/mount/$name"
ditto "$app" "$work/mount/$name"
codesign --verify --deep --strict "$work/mount/$name"
cdhash() { codesign -dvvv "$1" 2>&1 | sed -n 's/^CDHash=//p'; }
test -n "$(cdhash "$app")"
test "$(cdhash "$app")" = "$(cdhash "$work/mount/$name")"
for attempt in 1 2 3; do
  if hdiutil detach "$work/mount" -quiet; then mounted=''; break; fi
  sleep "$attempt"
done
if [ -n "$mounted" ]; then hdiutil detach "$work/mount" -force -quiet; mounted=''; fi
hdiutil convert "$work/image.sparseimage" -format UDZO -imagekey zlib-level=9 -o "$work/image.dmg" -quiet
mv -f "$work/image.dmg" "$dmg"
if [ "$mode" = signed ]; then
  codesign --force --timestamp --keychain "$keychain" --sign "$identity" "$dmg"
  codesign --verify --strict --verbose=2 "$dmg"
fi

# 4. Only the sidecar holds allow-jit, in the app and in the image.
check() {
  local bundle=$1 sidecar app
  # Captured first: grep -q closing the pipe early would fail under pipefail.
  sidecar=$(codesign -d --entitlements - --xml "$bundle/Contents/MacOS/cartograph-agent" 2>/dev/null)
  app=$(codesign -d --entitlements - --xml "$bundle" 2>/dev/null || true)
  case "$sidecar" in
    *"$jit"*) ;;
    *) echo "$bundle: the agent runtime is missing $jit" >&2; exit 1 ;;
  esac
  if [[ "$app" == *"$jit"* ]]; then
    echo "$bundle: the app must not hold $jit" >&2
    exit 1
  fi
  echo "$bundle: $jit on cartograph-agent only"
  # Claude Code ships as a resource, so nothing here re-signs it.
  codesign --verify --strict -R="=$claude_requirement" "$bundle/Contents/Resources/claude-code/claude" \
    || { echo "$bundle: Claude Code lost Anthropic's signature" >&2; exit 1; }
  echo "$bundle: Claude Code signed by $claude_team"
}
check "$app"
mkdir "$work/check"
hdiutil attach "$dmg" -nobrowse -noautoopen -readonly -mountpoint "$work/check" -quiet
mounted="$work/check"
check "$work/check/$name"
hdiutil detach "$work/check" -quiet || hdiutil detach "$work/check" -force -quiet
mounted=''
