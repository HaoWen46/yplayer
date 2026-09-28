import AppKit
import Observation
import SwiftUI
import YplayerKit

/// The menu-bar item and its popover. The popover's SwiftUI tree exists only while it is shown:
/// the hosting controller is created in `show()` and released in `popoverDidClose`.
@MainActor
final class StatusItemController: NSObject, NSPopoverDelegate {
    private let model: AppModel
    private let item: NSStatusItem
    private let popover = NSPopover()
    /// Whether the glyph currently shows `waveform`.
    private var showsPlaying = false

    init(model: AppModel) {
        self.model = model
        item = NSStatusBar.system.statusItem(withLength: NSStatusItem.squareLength)
        super.init()
        popover.behavior = .transient
        popover.animates = true
        popover.contentSize = NSSize(width: 360, height: 560)
        popover.delegate = self
        item.button?.image = Self.glyph(playing: false)
        item.button?.target = self
        item.button?.action = #selector(toggle(_:))
        observePlayState()
    }

    func show() {
        guard !popover.isShown, let button = item.button else { return }
        let root = PopoverView(model: model)
            .environment(\.closePopover) { [weak self] in self?.popover.performClose(nil) }
        let content = NSHostingController(rootView: root)
        content.sizingOptions = .preferredContentSize
        popover.contentViewController = content
        popover.show(relativeTo: button.bounds, of: button, preferredEdge: .minY)
        NSApp.activate()
    }

    func popoverDidClose(_ notification: Notification) {
        popover.contentViewController = nil
    }

    @objc private func toggle(_ sender: NSStatusBarButton) {
        if popover.isShown {
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
