#!/bin/bash
# tako-core — TakoCoreUI tests on macOS
# Copyright (c) 2026 Alexander Panasenko
# Contact: alex@prod.codes
# Author: https://prod.codes/about/
# Project: https://github.com/alex09x/tako-core
# SPDX-License-Identifier: MIT
#
# Runs the TakoCoreUI tests against the engine built from this checkout, not
# the release asset Package.swift names for consumers.
#
#   scripts/swift-test.sh                         # every TakoCoreUI test
#   scripts/swift-test.sh --filter MetalKitty     # extra arguments go to swift test
#
# The macOS slice is rebuilt only when the engine sources changed since the
# last build (by content; scripts/engine_fingerprint.py says why not by time).
set -euo pipefail
cd "$(dirname "$0")/.."

# An --ios-only build restamps the framework without a macOS slice, so the
# slice itself is checked too.
if ! python3 scripts/engine_fingerprint.py check TakoCore.xcframework \
   || ! python3 scripts/engine_fingerprint.py check target/macos/libtako_core.a \
   || [ ! -f TakoCore.xcframework/macos-arm64/libtako_core.a ]; then
    ./scripts/build-xcframework.sh --macos-only
fi

# SwiftPM's synthetic all-products test bundle does not pick up the binary
# target's library on every toolchain; passing it to the linker directly
# makes every link step see the same engine.
#
# Serially: several tests share process-wide state (the log level, the
# pasteboard, WindowServer), and Swift Testing's parallel scheduling lets one
# test's setting leak into another's assertions.
export TAKOCORE_LOCAL_XCFRAMEWORK=1
exec swift test --no-parallel -Xlinker -Ltarget/macos -Xlinker -ltako_core "$@"
