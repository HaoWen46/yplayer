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
    /// The service's settings: fetched when the settings window opens and after each reconnect
    /// while it is open, and kept current by `settings` events; nil until the first fetch.
    var settings: ServiceSettings?
    /// The music folder move started from the settings window, while it runs or after it failed.
    var folderMove: FolderMove?
    /// Whether the settings window is open (set by `SettingsWindowController`).
    @ObservationIgnored var settingsShown = false
    /// Opens the settings window (set by `AppDelegate`).
    @ObservationIgnored var openSettings: @MainActor () -> Void = {}
    /// The pending destructive action, shown by `ConfirmOverlay`.
    var confirm: ConfirmRequest?
    /// Whether `LyricsView` replaces the library below the now-playing card.
    var showsLyrics = false
    /// Whether `UpNextView` replaces the library below the now-playing card (never together with
    /// `showsLyrics`).
    var showsQueue = false

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
                await applySettings(update)
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

    /// Removes Up Next entry `index` of `section`, which must be `trackID`.
    func removeFromQueue(_ section: QueueSection, _ index: Int, _ trackID: String) async {
        await send(.queueRemove(section: section, index: index, trackID: trackID))
    }

    /// Moves play-next entry `from` (`trackID`) so it ends at index `to`.
    func moveInQueue(from: Int, to: Int, _ trackID: String) async {
        await send(.queueMove(from: from, to: to, trackID: trackID))
    }

    /// Empties Playing Next.
    func clearQueue() async {
        await send(.queueClear)
    }

    /// Plays Up Next entry `index` of `section` (`trackID`) now.
    func jumpInQueue(_ section: QueueSection, _ index: Int, _ trackID: String) async {
        await send(.queueJump(section: section, index: index, trackID: trackID))
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
            store.appendToast(ToastItem(severity: .warn, message: "The song's file is missing."))
            return
        }
        NSWorkspace.shared.activateFileViewerSelecting([URL(filePath: path)])
    }

    func copyLink(_ trackID: String) async {
        guard YouTubeURL.videoID(from: trackID) == .success(trackID) else { return }
        NSPasteboard.general.clearContents()
        NSPasteboard.general.setString(
            "https://www.youtube.com/watch?v=\(trackID)", forType: .string)
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
            await send(.trackDelete(trackID: result.trackID, toTrash: true))
        }
    }

    /// Replaces the store's library and Up Next with the service's.
    private func reload() async {
        do {
            store.load(try await client.send(.libraryGet, as: LibrarySnapshot.self))
            store.loadQueue(try await client.send(.queueGet, as: QueueState.self))
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
        store.appendToast(ToastItem(severity: .error, message: message))
    }
}

/// A music folder move started from the settings window.
enum FolderMove: Equatable {
    /// The service accepted `library.move` and restarts to move the folder to `target`.
    case moving(target: String)
    /// The move was refused or did not happen; the message is shown under the folder.
    case failed(String)
}

extension AppModel {
    /// Fetches `settings`; returns false (and keeps them) when the service can't be reached.
    @discardableResult
    func loadSettings() async -> Bool {
        do {
            settings = try await client.send(.settingsGet, as: ServiceSettings.self)
            return true
        } catch {
            return false
        }
    }

    /// Sends `settings.set` with the fields given and applies the reply; returns the error
    /// message, or nil on success.
    func changeSettings(levelLoudness: Bool? = nil, apiKey: String?? = nil) async -> String? {
        do {
            settings = try await client.send(
                .settingsSet(levelLoudness: levelLoudness, apiKey: apiKey),
                as: ServiceSettings.self)
            return nil
        } catch {
            return Self.message(error)
        }
    }

    /// Asks the service to move the music folder to `target`; `folderMove` follows the move
    /// until the restarted service reports its folder.
    func moveMusicFolder(to target: String) async {
        folderMove = .moving(target: target)
        do {
            _ = try await client.send(.libraryMove(to: target), as: LibraryMoveResult.self)
        } catch {
            folderMove = .failed(Self.message(error))
        }
    }

    /// Applies a `settings` event. After a reconnect, fetches the settings again while the window
    /// is open or a move runs, and ends the move: done when the service reports `target`.
    fileprivate func applySettings(_ update: ClientUpdate) async {
        switch update {
        case .event(.settings(let new)):
            settings = new
        case .connected:
            let moving: String? =
                if case .moving(let target) = folderMove { target } else { nil }
            guard settingsShown || moving != nil, await loadSettings(), let moving else { return }
            folderMove =
                settings?.musicFolder == moving ? nil : .failed("Couldn't move your music folder.")
        default:
            break
        }
    }

    private static func message(_ error: any Error) -> String {
        (error as? ServiceError)?.message ?? error.localizedDescription
    }
}
