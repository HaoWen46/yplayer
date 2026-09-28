import AppKit
import SwiftUI
import YplayerKit

/// `--snapshot-dir`: renders every debug state and writes `<dir>/<name>.png` (2x on a Retina
/// display). `ImageRenderer` and `cacheDisplay` both drop Liquid Glass on macOS (`ImageRenderer`
/// also drops buttons and scroll views), so each state is shown in a borderless window below the
/// desktop, where the user never sees it, and captured by the window server with
/// `screencapture -l`.
@MainActor
enum SnapshotRenderer {
    struct Failure: Error, CustomStringConvertible {
        let description: String
    }

    static let size = NSSize(width: 360, height: 560)

    /// The rendered states, by file name.
    static func states() -> [(name: String, view: AnyView)] {
        [
            ("popover-disconnected", popover(DebugFixtures.disconnectedModel())),
            ("popover-confirm-delete", popover(DebugFixtures.confirmDeleteModel())),
            ("popover-empty", popover(DebugFixtures.emptyModel())),
            ("card-playing", card(DebugFixtures.artworkModel(fixtureArtwork()))),
            ("card-paused", card(DebugFixtures.pausedModel(fixtureArtwork()))),
            ("card-nothing", card(DebugFixtures.emptyModel())),
            (
                "popover-lyrics",
                lyricsPopover(DebugFixtures.artworkModel(fixtureArtwork()), DebugFixtures.lyrics)
            ),
            (
                "lyrics-missing",
                lyricsPopover(DebugFixtures.artworkModel(fixtureArtwork()), .missing)
            ),
            (
                "popover-albums",
                popover(
                    DebugFixtures.model(DebugFixtures.store()), route: LibraryRoute(tab: .albums))
            ),
            (
                "popover-album-detail",
                popover(
                    DebugFixtures.model(DebugFixtures.store()),
                    route: LibraryRoute(tab: .albums, albumID: 2))
            ),
            (
                "popover-songs",
                popover(DebugFixtures.songsModel(), route: LibraryRoute(tab: .songs))
            ),
            (
                "popover-search",
                popover(DebugFixtures.searchModel(), route: LibraryRoute(tab: .search, query: "はむ"))
            ),
        ]
    }

    static func render(to dir: URL) async throws {
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        for (name, view) in states() {
            let url = dir.appending(path: "\(name).png")
            try await capture(view, to: url)
            print(url.path(percentEncoded: false))
        }
    }

    /// The popover over the window background, as in an active app.
    private static func popover(_ model: AppModel) -> AnyView {
        AnyView(
            PopoverView(model: model)
                .background(.windowBackground)
                .environment(\.appearsActive, true))
    }

    /// The popover with the library at `route`.
    private static func popover(_ model: AppModel, route: LibraryRoute) -> AnyView {
        AnyView(popover(model).environment(\.initialLibraryRoute, route))
    }

    private static func capture(_ view: AnyView, to url: URL) async throws {
        let window = NSWindow(
            contentRect: NSRect(origin: .zero, size: size), styleMask: .borderless,
            backing: .buffered, defer: false)
        window.isReleasedWhenClosed = false
        window.level = NSWindow.Level(rawValue: Int(CGWindowLevelForKey(.desktopWindow)) - 1)
        window.contentView = NSHostingView(rootView: view)
        window.orderFrontRegardless()
        defer { window.orderOut(nil) }
        try await Task.sleep(for: .milliseconds(500))
        let path = url.path(percentEncoded: false)
        try? FileManager.default.removeItem(atPath: path)
        let screencapture = Process()
        screencapture.executableURL = URL(filePath: "/usr/sbin/screencapture")
        screencapture.arguments = ["-x", "-o", "-l", String(window.windowNumber), path]
        try screencapture.run()
        screencapture.waitUntilExit()
        guard screencapture.terminationStatus == 0, FileManager.default.fileExists(atPath: path)
        else { throw Failure(description: "screencapture failed for \(url.lastPathComponent)") }
    }
}

extension SnapshotRenderer {
    /// A gradient JPEG in the temporary directory standing in for a track's `cover.jpg`.
    fileprivate static func fixtureArtwork() -> String? {
        let cover = ZStack {
            LinearGradient(
                colors: [.indigo, .purple, .pink, .orange], startPoint: .topLeading,
                endPoint: .bottomTrailing)
            Image(systemName: "moon.stars.fill")
                .font(.system(size: 180))
                .foregroundStyle(.white.opacity(0.85))
        }
        .frame(width: 480, height: 480)
        guard let image = ImageRenderer(content: cover).cgImage,
            let jpeg = NSBitmapImageRep(cgImage: image).representation(
                using: .jpeg, properties: [:])
        else { return nil }
        let url = FileManager.default.temporaryDirectory.appending(
            path: "yplayer-fixture-cover.jpg")
        guard (try? jpeg.write(to: url)) != nil else { return nil }
        return url.path(percentEncoded: false)
    }

    /// The now-playing card alone, at the top of a popover-sized window.
    fileprivate static func card(_ model: AppModel) -> AnyView {
        AnyView(
            NowPlayingCard(model: model)
                .padding(12)
                .frame(width: size.width, height: size.height, alignment: .top)
                .background(.windowBackground)
                .environment(\.appearsActive, true)
                .onAppear { restartClock(model.store) })
    }

    /// The popover with the lyrics view shown and `lyrics` in place of a fetch.
    fileprivate static func lyricsPopover(_ model: AppModel, _ lyrics: LyricsResult) -> AnyView {
        AnyView(
            popover(model)
                .environment(\.lyricsFixture, lyrics)
                .onAppear { restartClock(model.store) })
    }

    /// Restarts a playing fixture's clock at 1:02 when its state is shown (models are built before
    /// the first capture, so later captures would otherwise show a later position).
    private static func restartClock(_ store: LibraryStore) {
        guard store.player?.state == .playing else { return }
        store.apply(.event(.player(DebugFixtures.player())))
    }
}
