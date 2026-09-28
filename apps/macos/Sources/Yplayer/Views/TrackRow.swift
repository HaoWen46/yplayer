import SwiftUI
import YplayerKit

/// Where a track list lives: the library (Songs, Search) or an album.
enum TrackListContext {
    case library
    case album(Album)

    var album: Album? {
        if case .album(let album) = self { album } else { nil }
    }

    /// The play context for a row of this list.
    var ref: ContextRef {
        switch self {
        case .library: .library
        case .album(let album): .album(album.id)
        }
    }
}

/// One track: artwork, title, `uploader · m:ss` (the uploader truncates first), and a trailing
/// status (current track, download progress, failed). Reads its own track, player and download
/// state from the store, so a store change re-renders only the visible rows that read it, never
/// the list.
struct TrackRow: View {
    let model: AppModel
    let trackID: String
    @ViewState private var hovering = false

    var body: some View {
        HStack(spacing: 10) {
            if let track = model.store.tracks[trackID] {
                TrackArtwork(path: track.thumbPath)
                VStack(alignment: .leading, spacing: 1) {
                    Text(track.title)
                        .lineLimit(1)
                    HStack(spacing: 0) {
                        if let uploader = track.uploader {
                            Text(uploader)
                                .lineLimit(1)
                        }
                        if let seconds = track.duration {
                            Text((track.uploader == nil ? "" : " · ") + Self.time(seconds))
                                .fixedSize()
                        }
                    }
                    .font(.subheadline)
                    .foregroundStyle(.secondary)
                }
                Spacer(minLength: 4)
                status(track)
            }
        }
        .padding(.horizontal, 6)
        .padding(.vertical, 4)
        .background(
            .primary.opacity(hovering ? 0.06 : 0), in: .rect(cornerRadius: 8)
        )
        .contentShape(.rect)
        .onHover { hovering = $0 }
    }

    @ViewBuilder
    private func status(_ track: Track) -> some View {
        switch track.state {
        case .failed:
            Button {
                Task { await model.retry(track.id) }
            } label: {
                Image(systemName: "exclamationmark.triangle")
                    .foregroundStyle(.orange)
            }
            .buttonStyle(.borderless)
            .help("Download failed. Click to retry.")
        case .downloading:
            let progress = model.store.downloads[track.id]
            if let fraction = progress?.fraction, progress?.phase != .fetching {
                ProgressView(value: fraction)
                    .progressViewStyle(.circular)
                    .controlSize(.small)
            } else {
                ProgressView()
                    .controlSize(.small)
            }
        case .complete:
            if let player = model.store.player, player.trackID == track.id,
                player.state != .stopped
            {
                Image(systemName: "waveform")
                    .foregroundStyle(.tint)
            }
        }
    }

    /// `m:ss`.
    static func time(_ seconds: Int) -> String {
        String(format: "%d:%02d", seconds / 60, seconds % 60)
    }
}

/// A lazy list of track rows. Double-click or Return plays the row in this list's context; right
/// click shows `TrackContextMenu`; swipe left and ⌫ remove from the album (album list) or delete
/// from the library; ⌘⌫ deletes from the library. Every removal is confirmed. Album lists reorder
/// by drag (`album.reorder`). Space reaches `PopoverView`'s play/pause handler.
struct TrackList: View {
    let model: AppModel
    let trackIDs: [String]
    let context: TrackListContext
    @Environment(LibraryUI.self) private var ui
    @ViewState private var selection: String?

    var body: some View {
        list
            .contextMenu(forSelectionType: String.self) { ids in
                if let id = ids.first, let track = model.store.tracks[id] {
                    TrackContextMenu(model: model, ui: ui, track: track, album: context.album)
                }
            } primaryAction: { ids in
                if let id = ids.first { play(id) }
            }
            .onKeyPress(.return) {
                guard let id = selection else { return .ignored }
                play(id)
                return .handled
            }
            .onDeleteCommand {
                if let id = selection { remove(id, fromLibrary: context.album == nil) }
            }
            // ⌘⌫ reaches a focused list as `deleteToBeginningOfLine:`, never as a key press.
            .onCommand(#selector(NSStandardKeyBindingResponding.deleteToBeginningOfLine(_:))) {
                if let id = selection { remove(id, fromLibrary: true) }
            }
    }

    private var list: some View {
        let onMove: ((IndexSet, Int) -> Void)? = context.album == nil ? nil : { move($0, $1) }
        return List(selection: $selection) {
            ForEach(trackIDs, id: \.self) { id in
                row(id)
            }
            .onMove(perform: onMove)
        }
        .listStyle(.plain)
        .scrollContentBackground(.hidden)
    }

    private func row(_ id: String) -> some View {
        TrackRow(model: model, trackID: id)
            .listRowInsets(EdgeInsets(top: 1, leading: 0, bottom: 1, trailing: 0))
            .listRowSeparatorTint(Color.primary.opacity(0.06))
            .swipeActions(edge: .trailing, allowsFullSwipe: false) {
                Button {
                    remove(id, fromLibrary: context.album == nil)
                } label: {
                    if context.album == nil {
                        Label("Delete", systemImage: "trash")
                    } else {
                        Label("Remove", systemImage: "minus.circle")
                    }
                }
                .tint(.red)
            }
    }

    private func play(_ id: String) {
        Task { await model.play(trackID: id, context: context.ref) }
    }

    /// Asks to delete the track from the library, or to remove it from this list's album.
    private func remove(_ id: String, fromLibrary: Bool) {
        guard let track = model.store.tracks[id] else { return }
        if !fromLibrary, let album = context.album {
            LibraryConfirm.removeTrack(track, from: album, model: model)
        } else {
            LibraryConfirm.deleteTrack(track, model: model)
        }
    }

    private func move(_ offsets: IndexSet, _ destination: Int) {
        guard let album = context.album else { return }
        var ids = trackIDs
        ids.move(fromOffsets: offsets, toOffset: destination)
        Task { await model.reorderAlbum(album.id, ids) }
    }
}
