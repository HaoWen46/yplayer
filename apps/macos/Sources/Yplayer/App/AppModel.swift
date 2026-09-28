import AppKit
import Observation
import YplayerKit

/// A destructive action waiting for the user in `ConfirmOverlay`.
struct ConfirmRequest: Identifiable {
    let id = UUID()
    var title: String
    var message: String
    /// The destructive button's label.
    var actionTitle: String
    var action: @MainActor () async -> Void
}

/// The app's state and intents: owns the service connection and the store it feeds.
/// Intents never throw; their errors become toasts in `store`.
@MainActor
@Observable
final class AppModel {
    let store: LibraryStore
    let client: ServiceClient
    /// The pending destructive action, shown by `ConfirmOverlay`.
    var confirm: ConfirmRequest?
    /// Whether `LyricsView` replaces the library below the now-playing card.
    var showsLyrics = false

    init(client: ServiceClient, store: LibraryStore) {
        self.client = client
        self.store = store
    }

    /// Connects and applies the service's updates to `store` for the life of the app; call once.
    func start() {
        Task {
            await client.start()
            for await update in client.updates {
                store.apply(update)
                if case .connected = update {
                    await reload()
                } else if store.needsResync {
                    await reload()
                }
            }
        }
    }

    func play(trackID: String, context: ContextRef) async {
        await send(.play(trackID: trackID, context: context))
    }

    func toggle() async {
        await send(.toggle)
    }

    func next() async {
        await send(.next)
    }

    func prev() async {
        await send(.prev)
    }

    func seek(to position: Double) async {
        await send(.seek(position: position))
    }

    func setVolume(_ value: Double) async {
        await send(.volume(value: value))
    }

    /// none → all → single → shuffle → none.
    func cycleLoop() async {
        let mode: LoopMode =
            switch store.player?.loopMode ?? LoopMode.none {
            case .none: .all
            case .all: .single
            case .single: .shuffle
            case .shuffle: .none
            }
        await send(.loop(mode: mode))
    }

    func playNext(_ trackID: String) async {
        await send(.queuePlayNext(trackID: trackID))
    }

    @discardableResult
    func createAlbum(_ name: String) async -> Album? {
        do {
            return try await client.send(.albumCreate(name: name), as: AlbumCreateResult.self).album
        } catch {
            report(error)
            return nil
        }
    }

    func renameAlbum(_ albumID: Int64, _ name: String) async {
        await send(.albumRename(albumID: albumID, name: name))
    }

    func deleteAlbum(_ albumID: Int64) async {
        await send(.albumDelete(albumID: albumID))
    }

    func addToAlbum(_ trackID: String, _ albumID: Int64) async {
        await send(.albumAdd(albumID: albumID, trackID: trackID))
    }

    func removeFromAlbum(_ trackID: String, _ albumID: Int64) async {
        await send(.albumRemove(albumID: albumID, trackID: trackID))
    }

    func reorderAlbum(_ albumID: Int64, _ trackIDs: [String]) async {
        await send(.albumReorder(albumID: albumID, trackIDs: trackIDs))
    }

    /// Deletes the track from the library; its file moves to the Trash.
    func deleteTrack(_ trackID: String) async {
        await send(.trackDelete(trackID: trackID, toTrash: true))
    }

    func retry(_ trackID: String) async {
        await send(.trackRetry(trackID: trackID))
    }

    func renameTrack(_ trackID: String, _ title: String) async {
        await send(.trackRename(trackID: trackID, title: title))
    }

    /// The track's lyrics, or nil when the request failed.
    func lyrics(for trackID: String) async -> LyricsResult? {
        do {
            return try await client.send(.lyrics(trackID: trackID), as: LyricsResult.self)
        } catch {
            report(error)
            return nil
        }
    }

    func showInFinder(_ trackID: String) async {
        guard let path = store.tracks[trackID]?.audioPath,
            FileManager.default.fileExists(atPath: path)
        else {
            store.toasts.append(ToastItem(severity: .warn, message: "The song's file is missing."))
            return
        }
        NSWorkspace.shared.activateFileViewerSelecting([URL(filePath: path)])
    }

    func copyLink(_ trackID: String) async {
        guard let track = store.tracks[trackID] else { return }
        let link = track.webpageURL ?? "https://www.youtube.com/watch?v=\(track.id)"
        NSPasteboard.general.clearContents()
        NSPasteboard.general.setString(link, forType: .string)
    }

    func add(url: String, target: OrbTarget) async -> AddResult? {
        let album: AlbumRef? =
            switch target {
            case .album(let id, _): .id(id)
            case .inbox, .newAlbum: nil
            }
        do {
            return try await client.send(
                .add(url: url, album: album, play: true), as: AddResult.self)
        } catch {
            report(error)
            return nil
        }
    }

    func undoAdd(_ result: AddResult) async {
        if !result.wasInAlbum {
            await send(.albumRemove(albumID: result.albumID, trackID: result.trackID))
        }
        if result.wasNew {
            await send(.trackDelete(trackID: result.trackID, toTrash: false))
        }
    }

    /// Replaces the store's library with the service's.
    private func reload() async {
        do {
            store.load(try await client.send(.libraryGet, as: LibrarySnapshot.self))
        } catch {
            report(error)
        }
    }

    private func send(_ command: Command) async {
        do {
            try await client.send(command)
        } catch {
            report(error)
        }
    }

    private func report(_ error: any Error) {
        let message = (error as? ServiceError)?.message ?? error.localizedDescription
        store.toasts.append(ToastItem(severity: .error, message: message))
    }
}
