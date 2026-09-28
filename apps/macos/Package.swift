// swift-tools-version:6.2
import PackageDescription

let package = Package(
    name: "Yplayer",
    platforms: [.macOS(.v26)],
    products: [
        .executable(name: "Yplayer", targets: ["Yplayer"])
    ],
    targets: [
        .target(name: "YplayerKit"),
        .executableTarget(name: "Yplayer", dependencies: ["YplayerKit"]),
        .testTarget(name: "YplayerKitTests", dependencies: ["YplayerKit"]),
    ]
)
