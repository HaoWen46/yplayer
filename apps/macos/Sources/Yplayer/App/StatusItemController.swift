import AppKit
import Observation
import SwiftUI
import YplayerKit

/// The menu-bar item and its popover. The popover (window, layers and SwiftUI tree) exists only
/// while it is shown: it is created in `show()` and released in `popoverDidClose`.
@MainActor
final class StatusItemController: NSObject, NSPopoverDelegate {
    private let model: AppModel
    private let item: NSStatusItem
    private var popover: NSPopover?
    /// Whether the glyph currently shows `waveform`.
    private var showsPlaying = false

    init(model: AppModel) {
        self.model = model
        item = NSStatusBar.system.statusItem(withLength: NSStatusItem.squareLength)
        super.init()
        item.button?.image = Self.glyph(playing: false)
        item.button?.target = self
        item.button?.action = #selector(toggle(_:))
        observePlayState()
    }

    /// Whether the popover is shown (the orb ignores drags that start then).
    var isPopoverShown: Bool { popover != nil }

    func show() {
        guard popover == nil, let button = item.button else { return }
        let popover = NSPopover()
        popover.behavior = .transient
        popover.animates = true
        popover.contentSize = NSSize(width: 360, height: 560)
        popover.delegate = self
        let root = PopoverView(model: model)
            .environment(\.closePopover) { [weak self] in self?.close() }
        let content = NSHostingController(rootView: root)
        content.sizingOptions = .preferredContentSize
        popover.contentViewController = content
        self.popover = popover
        popover.show(relativeTo: button.bounds, of: button, preferredEdge: .minY)
        NSApp.activate()
    }

    func close() {
        popover?.performClose(nil)
    }

    /// Drops the popover and the artwork cache, then hands freed pages back to the system so the
    /// closed app returns to its idle footprint.
    func popoverDidClose(_ notification: Notification) {
        popover?.contentViewController = nil
        popover = nil
        Task {
            await ArtworkLoader.shared.purge()
            malloc_zone_pressure_relief(nil, 0)
        }
    }

    @objc private func toggle(_ sender: NSStatusBarButton) {
        if let popover {
            popover.performClose(sender)
        } else {
            show()
        }
    }

    /// Swaps the glyph when the play state changes; re-arms itself after every change of `player`.
    private func observePlayState() {
        let playing = withObservationTracking {
            model.store.player?.state == .playing
        } onChange: {
            Task { @MainActor in self.observePlayState() }
        }
        guard playing != showsPlaying else { return }
        showsPlaying = playing
        item.button?.image = Self.glyph(playing: playing)
    }

    private static func glyph(playing: Bool) -> NSImage? {
        NSImage(
            systemSymbolName: playing ? "waveform" : "music.note",
            accessibilityDescription: playing ? "yplayer, playing" : "yplayer")
    }
}
