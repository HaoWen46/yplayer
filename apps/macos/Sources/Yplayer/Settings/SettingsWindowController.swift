import AppKit
import SwiftUI
import YplayerKit

/// The settings window. It exists only while it is open: created in `show()` and released in
/// `windowWillClose`. While it is open the app is a regular app (Dock icon, ⌘-Tab) and fetches
/// the settings on open and after each reconnect; ⌘W and Esc close it.
@MainActor
final class SettingsWindowController: NSObject, NSWindowDelegate {
    private let model: AppModel
    private var window: NSWindow?

    init(model: AppModel) {
        self.model = model
    }

    /// Opens the window (fetching the settings), or brings it to the front.
    func show() {
        if window == nil {
            let window = Self.makeWindow(
                content: SettingsView(
                    model: model,
                    chooseDestination: { [weak self] in await self?.chooseDestination() }))
            window.delegate = self
            window.center()
            self.window = window
            model.settingsShown = true
            Task { await model.loadSettings() }
        }
        NSApp.setActivationPolicy(.regular)
        window?.makeKeyAndOrderFront(nil)
        window?.makeFirstResponder(nil)
        NSApp.activate()
    }

    /// The titled window hosting `content` at its ideal size (also used by `--snapshot-dir`).
    static func makeWindow(content: some View) -> NSWindow {
        let host = NSHostingController(rootView: content)
        host.sizingOptions = .preferredContentSize
        let window = SettingsWindow(contentViewController: host)
        window.styleMask = [.titled, .closable, .fullSizeContentView]
        window.title = "Yplayer Settings"
        window.titlebarAppearsTransparent = true
        window.isReleasedWhenClosed = false
        window.setContentSize(host.view.fittingSize)
        return window
    }

    /// Drops the window and its SwiftUI tree, forgets a failed move's error and hides the Dock
    /// icon again.
    func windowWillClose(_ notification: Notification) {
        window?.contentViewController = nil
        window = nil
        model.settingsShown = false
        if case .failed = model.folderMove { model.folderMove = nil }
        NSApp.setActivationPolicy(.accessory)
    }

    /// The folder picked in an open panel sheet ("Move Here"); nil when cancelled.
    private func chooseDestination() async -> URL? {
        guard let window else { return nil }
        let panel = NSOpenPanel()
        panel.canChooseFiles = false
        panel.canChooseDirectories = true
        panel.canCreateDirectories = true
        panel.allowsMultipleSelection = false
        panel.prompt = "Move Here"
        return await panel.beginSheetModal(for: window) == .OK ? panel.url : nil
    }
}

/// Closes on ⌘W and Esc (unless an input method is composing), and gives ⌘X, ⌘C, ⌘V and ⌘A
/// to the focused field: the app has no main menu to provide them.
private final class SettingsWindow: NSWindow {
    override func sendEvent(_ event: NSEvent) {
        if event.type == .keyDown, event.keyCode == 53,
            event.modifierFlags.isDisjoint(with: [.command, .control, .option, .shift]),
            (firstResponder as? NSTextView)?.hasMarkedText() != true
        {
            performClose(nil)
            return
        }
        super.sendEvent(event)
    }

    override func performKeyEquivalent(with event: NSEvent) -> Bool {
        guard
            event.modifierFlags.intersection([.command, .control, .option, .shift]) == .command
        else { return super.performKeyEquivalent(with: event) }
        let action: Selector
        switch event.charactersIgnoringModifiers {
        case "w":
            performClose(nil)
            return true
        case "x": action = #selector(NSText.cut(_:))
        case "c": action = #selector(NSText.copy(_:))
        case "v": action = #selector(NSText.paste(_:))
        case "a": action = #selector(NSText.selectAll(_:))
        default: return super.performKeyEquivalent(with: event)
        }
        return NSApp.sendAction(action, to: nil, from: self)
    }
}
