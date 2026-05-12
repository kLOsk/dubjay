// swift-tools-version:5.9
//
// DubShared — Swift package wrapping the UniFFI-generated bindings and
// the DubCore.xcframework binary target.
//
// The Apple app target in `apple/project.yml` depends on this package
// (`product: DubCore`). Everything Swift-side that talks to the Rust
// core goes through `import DubCore`.

import PackageDescription

let package = Package(
    name: "DubShared",
    platforms: [
        .macOS(.v13)
    ],
    products: [
        .library(name: "DubCore", targets: ["DubCore"]),
    ],
    targets: [
        // Binary target — the xcframework that `scripts/build-xcframework.sh`
        // produces. Path is relative to this Package.swift.
        .binaryTarget(
            name: "DubCoreFFI",
            path: "../DubCore.xcframework"
        ),
        // Swift target — the generated UniFFI bindings.
        // Source files land in Sources/DubCore/Generated/ after running
        // the bootstrap script; until then this directory is empty.
        .target(
            name: "DubCore",
            dependencies: ["DubCoreFFI"],
            path: "Sources/DubCore"
        ),
    ]
)
