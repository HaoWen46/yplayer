import SwiftUI
import YplayerKit

/// A search field over `store.search` (case-, width- and kana-insensitive); results act like
/// Songs rows. The field is focused on appear and on every ⌘F.
struct SearchView: View {
    let model: AppModel
    @Environment(LibraryUI.self) private var ui
    @FocusState private var focused: Bool

    var body: some View {
        @Bindable var ui = ui
        VStack(spacing: 8) {
            HStack(spacing: 6) {
                Image(systemName: "magnifyingglass")
                    .foregroundStyle(.secondary)
                TextField("Search your library", text: $ui.query)
                    .textFieldStyle(.plain)
                    .focused($focused)
                if !ui.query.isEmpty {
                    Button {
                        ui.query = ""
                    } label: {
                        Image(systemName: "xmark.circle.fill")
                            .foregroundStyle(.secondary)
                    }
                    .buttonStyle(.plain)
                    .help("Clear")
                }
            }
            .padding(.horizontal, 10)
            .padding(.vertical, 7)
            .glassEffect(.regular, in: .capsule)
            results
                .frame(maxHeight: .infinity)
        }
        .task { focused = true }
        .onChange(of: ui.searchFocusRequest) { focused = true }
    }

    @ViewBuilder
    private var results: some View {
        let query = ui.query.trimmingCharacters(in: .whitespaces)
        if query.isEmpty {
            ContentUnavailableView(
                "Search Your Library", systemImage: "magnifyingglass",
                description: Text("Find songs by title or artist."))
        } else {
            let tracks = model.store.search(query)
            if tracks.isEmpty {
                ContentUnavailableView.search(text: query)
            } else {
                TrackList(model: model, trackIDs: tracks.map(\.id), context: .library)
            }
        }
    }
}
