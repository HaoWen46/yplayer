import AppKit

/// Notices URL drags in other apps without any permission: global monitors see their
/// `.leftMouseDragged` and `.leftMouseUp` events, and the drag pasteboard's types are read only
/// when its change count moves. While the orb is up for a drag and no mouse-up was seen, a 0.1 s
/// poll of `NSEvent.pressedMouseButtons` stands in for monitors that go quiet during the drag.
@MainActor
final class DragWatcher {
    private let pasteboard = NSPasteboard(name: .drag)
    private let logDrags: Bool
    private let isPopoverShown: @MainActor () -> Bool
    private let onDrag: @MainActor () -> Void
    private let onMouseUp: @MainActor () -> Void
    private var lastChangeCount: Int
    private var monitor: Any?
    private var poll: Task<Void, Never>?

    /// `onDrag` shows the orb for a URL or text drag (not while the popover is shown: those drags
    /// are reorders inside the app); `onMouseUp` ends it. `logDrags` writes each observed drag's
    /// pasteboard types to stderr.
    init(
        logDrags: Bool, isPopoverShown: @escaping @MainActor () -> Bool,
        onDrag: @escaping @MainActor () -> Void, onMouseUp: @escaping @MainActor () -> Void
    ) {
        self.logDrags = logDrags
        self.isPopoverShown = isPopoverShown
        self.onDrag = onDrag
        self.onMouseUp = onMouseUp
        lastChangeCount = pasteboard.changeCount
        monitor = NSEvent.addGlobalMonitorForEvents(matching: [.leftMouseDragged, .leftMouseUp]) {
            [weak self] event in
            let type = event.type
            MainActor.assumeIsolated {
                switch type {
                case .leftMouseDragged: self?.dragged()
                case .leftMouseUp: self?.mouseUp()
                default: break
                }
            }
        }
    }

    /// Stops the mouse-button poll (the orb is hidden).
    func stopPolling() {
        poll?.cancel()
        poll = nil
    }

    private func dragged() {
        let count = pasteboard.changeCount
        guard count != lastChangeCount else { return }
        lastChangeCount = count
        let types = pasteboard.types ?? []
        if logDrags {
            let list = types.map(\.rawValue).joined(separator: ", ")
            FileHandle.standardError.write(Data("drag types: \(list)\n".utf8))
        }
        guard !isPopoverShown(), types.contains(.URL) || types.contains(.string) else { return }
        onDrag()
        poll?.cancel()
        poll = Task { [weak self] in
            while true {
                do {
                    try await Task.sleep(for: .milliseconds(100))
                } catch {
                    return
                }
                if NSEvent.pressedMouseButtons & 1 == 0 {
                    self?.mouseUp()
                    return
                }
            }
        }
    }

    /// The drag that showed the orb ended.
    private func mouseUp() {
        guard poll != nil else { return }
        stopPolling()
        onMouseUp()
    }
}
