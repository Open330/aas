// swift-tools-version:5.9
import PackageDescription

let package = Package(
    name: "AasBar",
    platforms: [.macOS(.v14)],
    targets: [
        .executableTarget(
            name: "AasBar",
            path: "Sources/AasBar",
            resources: [.process("Resources")]
        )
    ]
)
