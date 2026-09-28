import SwiftUI
import YplayerKit

/// Placeholder (replaced in S7): the library's titles, or an empty state.
struct LibraryTabs: View {
    let model: AppModel

    var body: some View {
        let store = model.store
        if store.libraryOrder.isEmpty {
            ContentUnavailableView(
                "No Songs", systemImage: "music.note.list",
                description: Text("Add one with `yplay add <url>`.")
            )
            .frame(maxHeight: .infinity)
        } else {
            ScrollView {
                LazyVStack(alignment: .leading, spacing: 0) {
                    ForEach(store.libraryOrder, id: \.self) { id in
                        Text(store.tracks[id]?.title ?? id)
                            .lineLimit(1)
                            .padding(.vertical, 6)
                    }
                }
                .padding(.horizontal, 4)
            }
        }
    }
}
