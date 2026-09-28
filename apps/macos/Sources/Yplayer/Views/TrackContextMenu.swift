import SwiftUI
import YplayerKit

/// The confirmations every destructive library action goes through (`ConfirmOverlay`).
@MainActor
enum LibraryConfirm {
    static func deleteTrack(_ track: Track, model: AppModel) {
        let size = track.fileSize.map { $0.formatted(.byteCount(style: .file)) + " " } ?? ""
        model.confirm = ConfirmRequest(
            title: "Delete “\(track.title)” from your library?",
            message: "The \(size)file moves to the Trash.", actionTitle: "Delete"
        ) {
            await model.deleteTrack(track.id)
        }
    }

    static func removeTrack(_ track: Track, from album: Album, model: AppModel) {
        model.confirm = ConfirmRequest(
            title: "Remove “\(track.title)” from \(album.name)?",
            message: "The song stays in your library.", actionTitle: "Remove"
        ) {
            await model.removeFromAlbum(track.id, album.id)
        }
    }

    static func deleteAlbum(_ album: Album, model: AppModel) {
        model.confirm = ConfirmRequest(
            title: "Delete album “\(album.name)”?",
            message: "Its songs stay in your library.", actionTitle: "Delete"
        ) {
            await model.deleteAlbum(album.id)
        }
    }
}

/// A track row's context menu; `album` is set in an album's list.
struct TrackContextMenu: View {
    let model: AppModel
    let ui: LibraryUI
    let track: Track
    let album: Album?

    var body: some View {
        Button("Play Next") {
            Task { await model.playNext(track.id) }
        }
        Divider()
        Menu("Add to Album") {
            ForEach(model.store.albums) { album in
                Button(album.name) {
                    Task { await model.addToAlbum(track.id, album.id) }
                }
            }
            if !model.store.albums.isEmpty {
                Divider()
            }
            Button("New Album…") {
                ui.prompt = NamePrompt(title: "New Album", initial: "", actionTitle: "Create") {
                    [model, track] name in
                    if let album = await model.createAlbum(name) {
                        await model.addToAlbum(track.id, album.id)
                    }
                }
            }
        }
        if let album {
            Button("Remove from Album…") {
                LibraryConfirm.removeTrack(track, from: album, model: model)
            }
        }
        Button("Delete from Library…", role: .destructive) {
            LibraryConfirm.deleteTrack(track, model: model)
        }
        Divider()
        Button("Show in Finder") {
            Task { await model.showInFinder(track.id) }
        }
        Button("Copy YouTube Link") {
            Task { await model.copyLink(track.id) }
        }
        Divider()
        if track.state == .failed {
            Button("Retry") {
                Task { await model.retry(track.id) }
            }
        }
        Button("Rename…") {
            ui.prompt = NamePrompt(
                title: "Rename Song", initial: track.title, actionTitle: "Rename"
            ) {
                [model, track] title in
                await model.renameTrack(track.id, title)
            }
        }
    }
}
