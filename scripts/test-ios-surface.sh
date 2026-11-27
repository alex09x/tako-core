#!/usr/bin/env bash
# tako-core — TakoCoreUI tests on the iOS Simulator
# Copyright (c) 2026 Alexander Panasenko
# Contact: alex@prod.codes
# Author: https://prod.codes/about/
# Project: https://github.com/alex09x/tako-core
# SPDX-License-Identifier: MIT
#
# Compiles TakoCoreUI and its XCTest cases for the iOS 17 Simulator against
# the engine built from this checkout, and runs them in a booted simulator.
# Swift Testing suites run on the Mac (scripts/swift-test.sh).
#
#   scripts/test-ios-surface.sh                     # builds the engine if needed
#   TAKO_IOS_SURFACE_DEVICE=<udid> scripts/test-ios-surface.sh

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

LIB_PATH="$ROOT_DIR/target/aarch64-apple-ios-sim/release/libtako_core.a"
HEADERS_DIR="$ROOT_DIR/target/bindings"
# Rebuilt unless the full framework was built from exactly these sources (by
# content, see scripts/engine_fingerprint.py); a --macos-only build has no
# simulator slice.
if ! python3 scripts/engine_fingerprint.py check TakoCore.xcframework \
   || [ -e TakoCore.xcframework.macos-only ] \
   || [ ! -f "$LIB_PATH" ] || [ ! -f "$HEADERS_DIR/tako_coreFFI.h" ]; then
    ./scripts/build-xcframework.sh
fi

# Rust's Apple target defaults do not reach C dependencies compiled by build
# scripts: without IPHONEOS_DEPLOYMENT_TARGET their objects can inherit the
# SDK's own minimum while the package claims iOS 17. Check every member.
TOO_NEW_MINIMUMS="$(otool -l "$LIB_PATH" 2>/dev/null \
    | awk '/^[[:space:]]+minos / && $2 + 0 > 17.0 { print $2 }' \
    | sort -Vu)"
if [[ -n "$TOO_NEW_MINIMUMS" ]]; then
    echo "the iOS engine archive has objects requiring newer than iOS 17:" >&2
    echo "$TOO_NEW_MINIMUMS" >&2
    exit 1
fi

# Per run, so two runs on different simulators cannot replace each other's
# runner before `simctl spawn` loads it.
mkdir -p "$ROOT_DIR/target"
BUILD_DIR="$(mktemp -d "$ROOT_DIR/target/ios_test_build.XXXXXX")"
trap 'rm -rf "$BUILD_DIR"' EXIT

SDK_PATH="$(xcrun --sdk iphonesimulator --show-sdk-path)"
PLATFORM_PATH="$(xcrun --sdk iphonesimulator --show-sdk-platform-path)"
SWIFT_UI_FILES=()
while IFS= read -r -d '' file; do SWIFT_UI_FILES+=("$file"); done \
    < <(find "$ROOT_DIR/swift/Sources/TakoCoreUI" -maxdepth 1 -name '*.swift' -print0)
GENERATED_SWIFT="$HEADERS_DIR/tako_core.swift"

echo "== TakoCoreUI for the iOS 17 Simulator"
swiftc -target arm64-apple-ios17.0-simulator \
    -sdk "$SDK_PATH" \
    -enable-testing \
    -module-name TakoCoreUI \
    -emit-module -emit-module-path "$BUILD_DIR/TakoCoreUI.swiftmodule" \
    -parse-as-library \
    -emit-library -o "$BUILD_DIR/libTakoCoreUI.dylib" \
    -Xlinker -install_name -Xlinker "$BUILD_DIR/libTakoCoreUI.dylib" \
    -Xcc -I"$HEADERS_DIR" \
    -import-objc-header "$HEADERS_DIR/tako_coreFFI.h" \
    "${SWIFT_UI_FILES[@]}" \
    "$GENERATED_SWIFT" \
    "$LIB_PATH" \
    -framework UIKit -framework Foundation -framework CoreText

cat << 'EOF' > "$BUILD_DIR/main.swift"
import XCTest
import TakoCoreUI
import UIKit

class TestObserver: NSObject, XCTestObservation {
    var failureCount = 0
    var totalCount = 0

    func testCaseDidFinish(_ testCase: XCTestCase) {
        totalCount += 1
        if testCase.testRun?.hasSucceeded == false {
            failureCount += 1
            print("FAIL: \(testCase.name)")
        }
    }

    func testCase(_ testCase: XCTestCase, didFailWithDescription description: String, inFile filePath: String?, atLine lineNumber: Int) {
        print("  \(description) at \(filePath ?? "?"):\(lineNumber)")
    }
}

let observer = TestObserver()
XCTestObservationCenter.shared.addTestObserver(observer)

let suite = XCTestSuite.default
print("== Running \(suite.testCaseCount) XCTest cases on the iOS Simulator")
suite.run()

print("Completed \(observer.totalCount) of \(suite.testCaseCount) tests with \(observer.failureCount) failures")
// Every discovered case has to finish, and the suite itself has to pass:
// a suite-level failure is reported to no single case.
let passed = observer.failureCount == 0
    && observer.totalCount > 0
    && observer.totalCount == suite.testCaseCount
    && suite.testRun?.hasSucceeded == true
exit(passed ? 0 : 1)
EOF

# XCTest files only: the Swift Testing suites run on the Mac.
SWIFT_TEST_FILES=()
while IFS= read -r -d '' file; do
    grep -q '^import Testing' "$file" || SWIFT_TEST_FILES+=("$file")
done < <(find "$ROOT_DIR/swift/Tests/TakoCoreUITests" -name '*.swift' -print0)

echo "== The XCTest runner for the iOS 17 Simulator"
swiftc -target arm64-apple-ios17.0-simulator \
    -continue-building-after-errors \
    -sdk "$SDK_PATH" \
    -F "$PLATFORM_PATH/Developer/Library/Frameworks" \
    -I "$PLATFORM_PATH/Developer/usr/lib" \
    -L "$PLATFORM_PATH/Developer/usr/lib" \
    -Xlinker -rpath -Xlinker "$PLATFORM_PATH/Developer/Library/Frameworks" \
    -Xlinker -rpath -Xlinker "$PLATFORM_PATH/Developer/usr/lib" \
    -Xlinker -rpath -Xlinker "$BUILD_DIR" \
    -I "$BUILD_DIR" \
    -L "$BUILD_DIR" \
    -lTakoCoreUI \
    -o "$BUILD_DIR/test_runner" \
    "${SWIFT_TEST_FILES[@]}" \
    "$BUILD_DIR/main.swift" \
    -framework UIKit -framework Foundation -framework CoreText -framework XCTest

# A shared Mac may have an operator's simulator booted; the caller can name
# its own with TAKO_IOS_SURFACE_DEVICE.
# Otherwise an available iPhone or iPad on iOS 17 or newer, a booted one first.
SIM_DEVICE="${TAKO_IOS_SURFACE_DEVICE:-}"
if [ -z "$SIM_DEVICE" ]; then
    SIM_DEVICE="$(xcrun simctl list devices available --json | python3 -c '
import json, re, sys
found = []
for runtime, devices in json.load(sys.stdin)["devices"].items():
    m = re.search(r"SimRuntime\.iOS-(\d+)", runtime)
    if not m or int(m.group(1)) < 17:
        continue
    for d in devices:
        if d.get("isAvailable") and re.match(r"iP(hone|ad)", d.get("name", "")):
            found.append((d.get("state") != "Booted", d["udid"]))
print(sorted(found)[0][1] if found else "")
')"
    if [ -z "$SIM_DEVICE" ]; then
        echo "no available iOS 17+ simulator" >&2
        exit 1
    fi
    xcrun simctl boot "$SIM_DEVICE" 2>/dev/null || true
fi

echo "== Running on simulator $SIM_DEVICE"
xcrun simctl spawn "$SIM_DEVICE" "$BUILD_DIR/test_runner"
