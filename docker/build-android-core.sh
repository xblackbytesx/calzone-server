#!/usr/bin/env bash
# Builds what the Android app consumes from this repo, into dist/:
#   dist/android/jniLibs/<abi>/libcalzone_ffi.so   stripped release libs (cargo-ndk)
#   dist/android/kotlin/uniffi/calzone_ffi/*.kt    generated Kotlin bindings
#   dist/host/libcalzone_ffi.so                    host lib for the app's JVM tests
# Bindings come from an UNSTRIPPED debug build: UniFFI's library mode reads
# metadata symbols that the release profile's `strip = true` removes.
set -euo pipefail
cd "$(dirname "$0")/.."

API="${ANDROID_API:-35}"   # = the app's minSdk (decision 44)
T="${CARGO_TARGET_DIR:-target}"
rm -rf dist/android dist/host
mkdir -p dist/android/kotlin dist/host

cargo build -p calzone-ffi
cargo run -q -p calzone-ffi --features cli --bin uniffi-bindgen -- \
  generate --library "$T/debug/libcalzone_ffi.so" --language kotlin --no-format --out-dir dist/android/kotlin

cargo build -p calzone-ffi --release
cp "$T/release/libcalzone_ffi.so" dist/host/

if [ "${HOST_ONLY:-0}" != "1" ]; then
  cargo ndk --platform "$API" -t arm64-v8a -t x86_64 -o dist/android/jniLibs build -p calzone-ffi --release
fi

# Hand ownership back to the host user when run as root in the container.
owner="$(stat -c '%u:%g' . 2>/dev/null || true)"
[ -n "$owner" ] && chown -R "$owner" dist 2>/dev/null || true
find dist -type f | sort
