// swift-tools-version:5.9
// Bird's menu bar app. Build it into build/Bird.app with ./build.sh.
import PackageDescription

let package = Package(
    name: "BirdBar",
    platforms: [.macOS(.v13)],
    targets: [
        .executableTarget(name: "BirdBar", path: "Sources/BirdBar"),
    ]
)
