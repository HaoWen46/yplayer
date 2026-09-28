import SwiftUI
import YplayerKit

/// Every song in library order; rows play in the library context, and ⌫ deletes from the
/// library.
struct SongsView: View {
    let model: AppModel

    var body: some View {
        let order = model.store.libraryOrder
        if order.isEmpty {
            ContentUnavailableView(
                "No Songs", systemImage: "music.note.list",
                description: Text("Add one with `yplay add <url>`.")
            )
            .frame(maxHeight: .infinity)
        } else {
            TrackList(model: model, trackIDs: order, context: .library)
        }
    }
}
