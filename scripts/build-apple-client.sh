#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
APPLE_DIR="${ROOT_DIR}/apple"
DERIVED_DATA="${APPLE_DIR}/Build/DerivedData"
CONFIGURATION="${CONFIGURATION:-Debug}"
JOBS="${STEVE_BUILD_JOBS:-6}"

if [[ -z "${DEVELOPER_DIR:-}" && -d "/Applications/Xcode.app/Contents/Developer" ]]; then
  export DEVELOPER_DIR="/Applications/Xcode.app/Contents/Developer"
fi

TARGET="${1:-all}"

build_web() {
  echo "==> Building Web UI (web/dist)"
  (cd "${ROOT_DIR}" && npm run build)
}

ensure_web_dist() {
  if [[ ! -f "${ROOT_DIR}/web/dist/index.html" ]]; then
    build_web
  fi
}

test_swift_package() {
  echo "==> Running SteveClientKit unit tests"
  swift test --package-path "${APPLE_DIR}/Packages/SteveClientKit" -j "${JOBS}"
}

build_mac() {
  ensure_web_dist
  echo "==> Building SteveMac (${CONFIGURATION})"
  xcodebuild \
    -project "${APPLE_DIR}/SteveClient.xcodeproj" \
    -scheme SteveMac \
    -configuration "${CONFIGURATION}" \
    -derivedDataPath "${DERIVED_DATA}" \
    -jobs "${JOBS}" \
    CODE_SIGNING_ALLOWED=NO \
    build
}

build_ios_sim() {
  ensure_web_dist
  echo "==> Building SteveIOS for generic iOS Simulator (${CONFIGURATION})"
  xcodebuild \
    -project "${APPLE_DIR}/SteveClient.xcodeproj" \
    -scheme SteveIOS \
    -destination "generic/platform=iOS Simulator" \
    -configuration "${CONFIGURATION}" \
    -derivedDataPath "${DERIVED_DATA}" \
    -jobs "${JOBS}" \
    CODE_SIGNING_ALLOWED=NO \
    build
}

build_ios_device() {
  ensure_web_dist
  echo "==> Building SteveIOS for generic iOS device (${CONFIGURATION}, unsigned)"
  xcodebuild \
    -project "${APPLE_DIR}/SteveClient.xcodeproj" \
    -scheme SteveIOS \
    -destination "generic/platform=iOS" \
    -configuration "${CONFIGURATION}" \
    -derivedDataPath "${DERIVED_DATA}" \
    -jobs "${JOBS}" \
    CODE_SIGNING_ALLOWED=NO \
    CODE_SIGNING_REQUIRED=NO \
    CODE_SIGN_IDENTITY="" \
    build
}

case "${TARGET}" in
  web)
    build_web
    ;;
  test)
    test_swift_package
    ;;
  mac)
    build_mac
    ;;
  ios-sim)
    build_ios_sim
    ;;
  ios)
    build_ios_device
    ;;
  all)
    build_web
    test_swift_package
    build_mac
    build_ios_device
    ;;
  *)
    echo "Usage: $0 [all|web|test|mac|ios-sim|ios]" >&2
    exit 1
    ;;
esac
