import SwiftUI

/// A row's 36 pt artwork for the cover at `path` (placeholder while it loads or when missing).
struct TrackArtwork: View {
    static let size: CGFloat = 36
    let path: String?

    var body: some View {
        ArtworkView(path: path, size: Self.size, cornerRadius: 6)
    }
}
