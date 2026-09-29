import AppKit
import YplayerKit

/// The orb panel's content view and drag destination (`.URL` and `.string`; not `.fileURL`).
/// `OrbView` is hosted on top of it; its own backing draws a near-invisible fill over the drop
/// zone (the core while armed, the whole bloom while bloomed) so the window server routes drags
/// there instead of through fully transparent pixels.
final class OrbDropView: NSView {
    private let state: OrbState
    weak var controller: OrbController?

    init(state: OrbState) {
        self.state = state
        super.init(frame: .zero)
        registerForDraggedTypes([.URL, .string])
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) {
        fatalError("init(coder:) is not supported")
    }

    override var isFlipped: Bool { true }

    override func draw(_ dirtyRect: NSRect) {
        let origin = state.layoutOrigin
        let layout = state.layout
        let zone: NSBezierPath
        switch state.phase {
        case .armed:
            zone = NSBezierPath(
                ovalIn: NSRect(
                    x: origin.x + layout.coreCenter.x - layout.core / 2,
                    y: origin.y + layout.coreCenter.y - layout.core / 2,
                    width: layout.core, height: layout.core))
        case .bloom:
            zone = NSBezierPath(rect: NSRect(origin: origin, size: layout.size))
        case .hidden, .shake, .toast:
            return
        }
        NSColor.black.withAlphaComponent(0.01).setFill()
        zone.fill()
    }

    override func draggingEntered(_ sender: any NSDraggingInfo) -> NSDragOperation {
        controller?.bloom()
        return .copy
    }

    override func draggingUpdated(_ sender: any NSDraggingInfo) -> NSDragOperation {
        let point = layoutPoint(sender)
        let hovered = state.layout.target(at: point)
        controller?.hover(hovered)
        return hovered != nil || state.layout.coreContains(point) ? .copy : []
    }

    override func draggingExited(_ sender: (any NSDraggingInfo)?) {
        controller?.collapse()
    }

    override func performDragOperation(_ sender: any NSDraggingInfo) -> Bool {
        let point = layoutPoint(sender)
        let target: OrbTarget
        if let index = state.layout.target(at: point) {
            target = state.targets.bubbles[index]
        } else if state.layout.coreContains(point) {
            target = state.targets.center
        } else {
            return false
        }
        let pasteboard = sender.draggingPasteboard
        let urls = (pasteboard.readObjects(forClasses: [NSURL.self]) as? [URL] ?? [])
            .map(\.absoluteString)
        let strings = pasteboard.pasteboardItems?.compactMap { $0.string(forType: .string) } ?? []
        let candidates = urls + strings
        guard let url = DropPayload.url(from: candidates) else {
            let playlist = candidates.contains {
                YouTubeURL.videoID(from: $0) == .failure(.playlistOnly)
            }
            controller?.reject(playlist ? "Playlists are not supported yet" : "Not a YouTube video")
            return false
        }
        controller?.drop(url: url, target: target)
        return true
    }

    /// The drag's location in `OrbLayout` coordinates.
    private func layoutPoint(_ sender: any NSDraggingInfo) -> CGPoint {
        let point = convert(sender.draggingLocation, from: nil)
        let origin = state.layoutOrigin
        return CGPoint(x: point.x - origin.x, y: point.y - origin.y)
    }
}
