// swift-tools-version: 5.10
import PackageDescription

let package = Package(
    name: "SteveClientKit",
    defaultLocalization: "zh-Hans",
    platforms: [
        .macOS(.v14),
        .iOS(.v17),
    ],
    products: [
        .library(
            name: "SteveClientKit",
            targets: ["SteveClientKit"]
        ),
    ],
    targets: [
        .target(
            name: "SteveClientKit"
        ),
        .testTarget(
            name: "SteveClientKitTests",
            dependencies: ["SteveClientKit"]
        ),
    ]
)
