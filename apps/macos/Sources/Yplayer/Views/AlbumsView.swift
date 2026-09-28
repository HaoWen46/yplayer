import SwiftUI
import YplayerKit

/// `@State` is a macro in this SDK whose plugin the Command Line Tools lack; the alias applies
/// the `State` property wrapper directly.
private typealias ViewState = State

/// The albums by name. + adds an inline name field (`album.create`); double-clicking a name
/// renames it inline; the context menu deletes an album (confirmed); clicking a row opens it.
/// Esc cancels an inline name (`LibraryUI`'s key monitor clears `ui.albumEdit`).
struct AlbumsView: View {
    let model: AppModel
    @Environment(LibraryUI.self) private var ui

    var body: some View {
        let albums = model.store.albums
        let creating = ui.albumEdit == .create
        VStack(spacing: 6) {
            HStack {
                Text(albums.count == 1 ? "1 Album" : "\(albums.count) Albums")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                Spacer()
                Button {
                    ui.albumEdit = .create
                } label: {
                    Image(systemName: "plus")
                        .font(.body.weight(.semibold))
                        .frame(width: 16, height: 16)
                }
                .buttonStyle(.glass)
                .buttonBorderShape(.circle)
                .help("New Album")
                .disabled(creating)
            }
            .padding(.horizontal, 14)
            if albums.isEmpty && !creating {
                ContentUnavailableView(
                    "No Albums", systemImage: "square.stack",
                    description: Text("Click + to create one.")
                )
                .frame(maxHeight: .infinity)
            } else {
                List {
                    if creating {
                        AlbumNameField(initial: "", placeholder: "New Album") { name in
                            guard ui.albumEdit == .create else { return }
                            ui.albumEdit = nil
                            guard let name else { return }
                            Task { await model.createAlbum(name) }
                        }
                        .listRowInsets(EdgeInsets(top: 1, leading: 0, bottom: 1, trailing: 0))
                        .listRowSeparator(.hidden)
                    }
                    ForEach(albums) { album in
                        row(album)
                            .listRowInsets(EdgeInsets(top: 1, leading: 0, bottom: 1, trailing: 0))
                            .listRowSeparatorTint(Color.primary.opacity(0.06))
                    }
                }
                .listStyle(.plain)
                .scrollContentBackground(.hidden)
            }
        }
    }

    @ViewBuilder
    private func row(_ album: Album) -> some View {
        if ui.albumEdit == .rename(album.id) {
            AlbumNameField(initial: album.name, placeholder: "Album Name") { name in
                guard ui.albumEdit == .rename(album.id) else { return }
                ui.albumEdit = nil
                guard let name, name != album.name else { return }
                Task { await model.renameAlbum(album.id, name) }
            }
        } else {
            AlbumRow(album: album) {
                ui.albumID = album.id
            } rename: {
                ui.albumEdit = .rename(album.id)
            }
            .contextMenu {
                Button("Delete Album…", role: .destructive) {
                    LibraryConfirm.deleteAlbum(album, model: model)
                }
            }
        }
    }

    /// "1 song" / "n songs".
    static func songCount(_ album: Album) -> String {
        album.trackIDs.count == 1 ? "1 song" : "\(album.trackIDs.count) songs"
    }
}

/// The 36 pt album glyph on a tinted rounded rect.
private struct AlbumIcon: View {
    var body: some View {
        RoundedRectangle(cornerRadius: 6)
            .fill(.tint.opacity(0.15))
            .overlay {
                Image(systemName: "square.stack")
                    .font(.system(size: 15))
                    .foregroundStyle(.tint)
            }
            .frame(width: TrackArtwork.size, height: TrackArtwork.size)
    }
}

/// Icon, name and song count; a click opens the album, a double click on the name renames it.
private struct AlbumRow: View {
    let album: Album
    let open: () -> Void
    let rename: () -> Void
    @ViewState private var hovering = false

    var body: some View {
        HStack(spacing: 10) {
            AlbumIcon()
            VStack(alignment: .leading, spacing: 1) {
                Text(album.name)
                    .lineLimit(1)
                    .gesture(
                        TapGesture(count: 2).onEnded(rename)
                            .exclusively(before: TapGesture().onEnded(open)))
                Text(AlbumsView.songCount(album))
                    .font(.subheadline)
                    .foregroundStyle(.secondary)
            }
            Spacer(minLength: 4)
            Image(systemName: "chevron.right")
                .font(.caption.weight(.semibold))
                .foregroundStyle(.tertiary)
        }
        .padding(.horizontal, 6)
        .padding(.vertical, 4)
        .background(.primary.opacity(hovering ? 0.06 : 0), in: .rect(cornerRadius: 8))
        .contentShape(.rect)
        .onTapGesture(perform: open)
        .onHover { hovering = $0 }
    }
}

/// An inline, focused name field. Return or losing focus finishes once with the trimmed name
/// (nil when empty).
private struct AlbumNameField: View {
    let placeholder: String
    let finish: (String?) -> Void
    @ViewState private var text: String
    @ViewState private var done = false
    @FocusState private var focused: Bool

    init(initial: String, placeholder: String, finish: @escaping (String?) -> Void) {
        self.placeholder = placeholder
        self.finish = finish
        _text = State(initialValue: initial)
    }

    var body: some View {
        HStack(spacing: 10) {
            AlbumIcon()
            TextField(placeholder, text: $text)
                .textFieldStyle(.roundedBorder)
                .focused($focused)
                .onSubmit(end)
        }
        .padding(.horizontal, 6)
        .padding(.vertical, 4)
        .onChange(of: focused) { _, isFocused in
            if !isFocused { end() }
        }
        .task { focused = true }
    }

    private func end() {
        guard !done else { return }
        done = true
        let name = text.trimmingCharacters(in: .whitespacesAndNewlines)
        finish(name.isEmpty ? nil : name)
    }
}
