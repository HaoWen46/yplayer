import SwiftUI
import YplayerKit

/// An album's songs in album order: back button and title above a list that plays in the
/// album's context, reorders by drag, and removes with swipe left or ⌫ (confirmed).
struct AlbumDetailView: View {
    let model: AppModel
    let album: Album
    @Environment(LibraryUI.self) private var ui

    var body: some View {
        VStack(spacing: 6) {
            HStack(spacing: 8) {
                Button {
                    ui.albumID = nil
                } label: {
                    Image(systemName: "chevron.left")
                        .font(.body.weight(.semibold))
                        .frame(width: 16, height: 16)
                }
                .buttonStyle(.glass)
                .buttonBorderShape(.circle)
                .help("Albums")
                VStack(alignment: .leading, spacing: 0) {
                    Text(album.name)
                        .font(.headline)
                        .lineLimit(1)
                    Text(AlbumsView.songCount(album))
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
                Spacer(minLength: 0)
            }
            .padding(.horizontal, 14)
            if album.trackIDs.isEmpty {
                ContentUnavailableView(
                    "No Songs", systemImage: "music.note",
                    description: Text("Add songs with Add to Album in a song's menu.")
                )
                .frame(maxHeight: .infinity)
            } else {
                TrackList(model: model, trackIDs: album.trackIDs, context: .album(album))
            }
        }
    }
}
