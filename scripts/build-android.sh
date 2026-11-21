#!/usr/bin/env bash
# Builds the Android app: the Rust core for each phone CPU, then the APK.
# Needs the Android SDK and NDK (ANDROID_HOME, default ~/Android/Sdk) and
# cargo-ndk (cargo install cargo-ndk).
# Usage: scripts/build-android.sh [debug|release|bundle]   (default release)
#   bundle: the Play Store App Bundle (.aab), signed with the upload key
#   from android/keystore.properties when present.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
KIND="${1:-release}"
export ANDROID_HOME="${ANDROID_HOME:-$HOME/Android/Sdk}"
if [[ -z "${ANDROID_NDK_HOME:-}" ]]; then
  ANDROID_NDK_HOME="$(ls -d "$ANDROID_HOME"/ndk/* 2>/dev/null | sort -V | tail -n 1)"
  export ANDROID_NDK_HOME
fi

cd "$ROOT"
cargo ndk -t arm64-v8a -t armeabi-v7a -t x86_64 -P 26 \
  -o android/app/src/main/jniLibs build --release -p hopwyn-mobile

cd android
[[ -f local.properties ]] || echo "sdk.dir=$ANDROID_HOME" > local.properties
if [[ "$KIND" == "debug" ]]; then
  ./gradlew assembleDebug --console=plain -q
  ls app/build/outputs/apk/debug/*.apk
elif [[ "$KIND" == "bundle" ]]; then
  [[ -f keystore.properties ]] || echo "warning: no android/keystore.properties; signing with the debug key (Play will refuse it)" >&2
  ./gradlew bundleRelease --console=plain -q
  ls app/build/outputs/bundle/release/*.aab
else
  ./gradlew assembleRelease --console=plain -q
  ls app/build/outputs/apk/release/*.apk
fi
