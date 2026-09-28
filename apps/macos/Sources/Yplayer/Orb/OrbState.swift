import Foundation
import Observation
import YplayerKit

/// "Added to <album>" after a drop; `removed` once Undo went through.
struct AddedToast: Equatable {
    let id = UUID()
    var album: String
    var result: AddResult
    var removed = false
}

/// What the orb shows.
enum OrbPhase: Equatable {
    case hidden
    /// A URL drag is on screen: the glass core alone.
    case armed
    /// The drag is over the orb: album bubbles around the core; `hovered` is a bubble index.
    case bloom(hovered: Int?)
    /// A rejected drop: the core shakes beside the message.
    case shake(String)
    case toast(AddedToast)
}

/// The orb's state: set by `OrbController` (and through it `OrbDropView`), drawn by `OrbView`.
@MainActor
@Observable
final class OrbState {
    /// The toast's room to the left of the orb; the canvas is at least this wide.
    static let toastWidth: CGFloat = 320

    var phase = OrbPhase.hidden
    /// The drop targets, fixed when the orb is shown.
    private(set) var targets = OrbTargets.make(albums: [])
    private(set) var layout = OrbLayout(targetCount: 1)
    /// A drop is being handled (naming or adding); the orb stays up.
    var dropInProgress = false

    /// The panel's content size: the bloom, widened to fit a toast.
    var canvasSize: CGSize {
        CGSize(width: max(layout.size.width, Self.toastWidth), height: layout.size.height)
    }

    /// The layout's origin in the canvas (top-left coordinates): the bloom sits at the right edge.
    var layoutOrigin: CGPoint {
        CGPoint(x: canvasSize.width - layout.size.width, y: 0)
    }

    func setTargets(_ targets: OrbTargets) {
        self.targets = targets
        layout = OrbLayout(targetCount: targets.bubbles.count)
    }

    func name(of target: OrbTarget) -> String {
        switch target {
        case .album(_, let name): name
        case .inbox: "Inbox"
        case .newAlbum: "New…"
        }
    }
}
