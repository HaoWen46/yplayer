import SwiftUI
import YplayerKit

/// Placeholder (replaced in S6): the current track's title and uploader.
struct NowPlayingCard: View {
    let model: AppModel

    var body: some View {
        let track = model.store.currentTrack
        HStack(spacing: 12) {
            RoundedRectangle(cornerRadius: 8)
                .fill(.tint.opacity(0.15))
                .frame(width: 64, height: 64)
                .overlay {
                    Image(systemName: "music.note")
                        .font(.title2)
                        .foregroundStyle(.tint)
                }
            VStack(alignment: .leading, spacing: 2) {
                Text(track?.title ?? "Not playing")
                    .font(.headline)
                    .lineLimit(1)
                if let uploader = track?.uploader {
                    Text(uploader)
                        .font(.subheadline)
                        .foregroundStyle(.secondary)
                        .lineLimit(1)
                }
            }
            Spacer(minLength: 0)
        }
        .padding(12)
        .glassEffect(.regular, in: .rect(cornerRadius: 16))
    }
}
