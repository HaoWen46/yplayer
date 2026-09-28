import Foundation

public enum OrbTarget: Hashable, Sendable {
    case album(id: Int64, name: String)
    case inbox
    case newAlbum
}

public struct OrbTargets: Equatable, Sendable {
    public var bubbles: [OrbTarget]
    public var center: OrbTarget

    public static func make(albums: [Album], maxBubbles: Int = 5) -> OrbTargets {
        let sorted = albums.sorted { a, b in
            switch (a.lastUsedAt, b.lastUsedAt) {
            case (let x?, let y?) where x != y: x > y
            case (.some, .none): true
            case (.none, .some): false
            default: a.name.localizedStandardCompare(b.name) == .orderedAscending
            }
        }
        let recent = sorted.first.flatMap { $0.lastUsedAt == nil ? nil : $0 }
        let center: OrbTarget = recent.map { .album(id: $0.id, name: $0.name) } ?? .inbox
        // The core already offers the center album, so the bubbles are the next ones.
        let bubbles = sorted.filter { $0.id != recent?.id }.prefix(max(maxBubbles, 0)).map {
            OrbTarget.album(id: $0.id, name: $0.name)
        }
        return OrbTargets(bubbles: bubbles + [.newAlbum], center: center)
    }
}
