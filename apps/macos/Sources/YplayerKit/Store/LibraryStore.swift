import Foundation
import Observation

/// The app's view of the service: library, player, downloads, connection and toasts.
@MainActor
@Observable
public final class LibraryStore {
    public private(set) var tracks: [String: Track] = [:]
    /// Track ids by `addedAt` descending, then title.
    public private(set) var libraryOrder: [String] = []
    /// Albums by name.
    public private(set) var albums: [Album] = []
    public private(set) var player: PlayerState?
    public private(set) var downloads: [String: DownloadProgress] = [:]
    public private(set) var connection: ConnectionStatus = .connecting
    public var toasts: [ToastItem] = []
    public private(set) var libraryVersion: UInt64 = 0
    /// Set by a `resync` event; cleared by `load`.
    public private(set) var needsResync = false

    /// Folded title and uploader per track id, for `search`.
    @ObservationIgnored private var folded: [String: String] = [:]

    public init() {}

    public var currentTrack: Track? {
        player?.trackID.flatMap { tracks[$0] }
    }

    public func tracks(in album: Album) -> [Track] {
        album.trackIDs.compactMap { tracks[$0] }
    }

    public func album(id: Int64) -> Album? {
        albums.first { $0.id == id }
    }

    /// Tracks whose folded title or uploader contains the folded query, in library order.
    public func search(_ query: String) -> [Track] {
        if query.isEmpty { return [] }
        let needle = SearchNormalizer.fold(query)
        return libraryOrder.compactMap { id in
            guard let haystack = folded[id], haystack.range(of: needle, options: .literal) != nil
            else { return nil }
            return tracks[id]
        }
    }

    public func load(_ snapshot: LibrarySnapshot) {
        tracks = Dictionary(snapshot.tracks.map { ($0.id, $0) }, uniquingKeysWith: { $1 })
        libraryOrder = tracks.values.sorted(by: Self.precedes).map(\.id)
        albums = snapshot.albums.sorted(by: Self.namePrecedes)
        folded = tracks.mapValues(Self.searchKey)
        libraryVersion = snapshot.libraryVersion
        needsResync = false
    }

    public func apply(_ update: ClientUpdate) {
        switch update {
        case .connecting:
            connection = .connecting
        case .connected(let result):
            connection = .connected
            player = result.player
        case .disconnected(let retryIn):
            let (seconds, attoseconds) = retryIn.components
            let interval = Double(seconds) + Double(attoseconds) / 1e18
            connection = .disconnected(retryAt: Date().addingTimeInterval(interval))
        case .event(let event):
            apply(event)
        }
    }

    private func apply(_ event: Event) {
        switch event {
        case .player(let state):
            applyPlayer(state)
        case .trackUpsert(let track):
            upsert(track)
        case .trackRemoved(let id):
            remove(trackID: id)
        case .albumUpsert(let album):
            if let index = albums.firstIndex(where: { $0.id == album.id }) {
                albums[index] = album
            } else {
                albums.append(album)
            }
            albums.sort(by: Self.namePrecedes)
        case .albumRemoved(let id):
            albums.removeAll { $0.id == id }
        case .download(let download):
            switch download.phase {
            case .done, .failed, .cancelled:
                downloads[download.trackID] = nil
            case .fetching, .downloading:
                var fraction: Double?
                if let bytes = download.bytes, let total = download.total, total > 0 {
                    fraction = Double(bytes) / Double(total)
                }
                downloads[download.trackID] = DownloadProgress(
                    phase: download.phase, fraction: fraction)
            }
        case .toast(let toast):
            toasts.append(ToastItem(severity: toast.severity, message: toast.message))
        case .resync:
            needsResync = true
        case .unknown:
            break
        }
    }

    /// Replaces `player` only on a real change: any field other than `position`/`atMs`, or a
    /// position more than 1 s away from the extrapolated clock.
    private func applyPlayer(_ new: PlayerState) {
        guard let old = player else {
            player = new
            return
        }
        var oldFields = old
        var newFields = new
        oldFields.position = 0
        oldFields.atMs = 0
        newFields.position = 0
        newFields.atMs = 0
        let expected = PositionClock.position(
            old, now: Date(timeIntervalSince1970: Double(new.atMs) / 1000))
        if oldFields != newFields || abs(new.position - expected) > 1 {
            player = new
        }
    }

    private func upsert(_ track: Track) {
        tracks[track.id] = track
        folded[track.id] = Self.searchKey(track)
        var order = libraryOrder
        order.removeAll { $0 == track.id }
        let index =
            order.firstIndex { id in
                guard let other = tracks[id] else { return false }
                return Self.precedes(track, other)
            } ?? order.endIndex
        order.insert(track.id, at: index)
        libraryOrder = order
    }

    private func remove(trackID id: String) {
        tracks[id] = nil
        folded[id] = nil
        libraryOrder.removeAll { $0 == id }
        for index in albums.indices where albums[index].trackIDs.contains(id) {
            albums[index].trackIDs.removeAll { $0 == id }
        }
        downloads[id] = nil
    }

    private static func precedes(_ a: Track, _ b: Track) -> Bool {
        let aAdded = a.addedAt ?? .min
        let bAdded = b.addedAt ?? .min
        if aAdded != bAdded { return aAdded > bAdded }
        return a.title.localizedStandardCompare(b.title) == .orderedAscending
    }

    private static func namePrecedes(_ a: Album, _ b: Album) -> Bool {
        a.name.localizedStandardCompare(b.name) == .orderedAscending
    }

    private static func searchKey(_ track: Track) -> String {
        SearchNormalizer.fold(track.title + "\n" + (track.uploader ?? ""))
    }
}
