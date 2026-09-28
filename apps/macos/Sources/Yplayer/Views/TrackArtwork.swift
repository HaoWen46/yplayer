import SwiftUI

/// A row's 36 pt artwork for the cover at `path`. Placeholder until the artwork loader is hooked
/// up: `music.note` on a tinted rounded rect.
struct TrackArtwork: View {
    static let size: CGFloat = 36
    let path: String?

    var body: some View {
        RoundedRectangle(cornerRadius: 6)
            .fill(.tint.opacity(0.15))
            .overlay {
                Image(systemName: "music.note")
                    .font(.system(size: 14))
                    .foregroundStyle(.tint)
            }
            .frame(width: Self.size, height: Self.size)
    }
}
