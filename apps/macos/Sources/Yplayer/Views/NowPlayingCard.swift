import SwiftUI
import YplayerKit

/// `@State` as the property wrapper: the macOS 27 SDK's `@State` macro needs the SwiftUIMacros
/// plugin, which Command Line Tools lack.
private typealias ViewState = SwiftUI.State

/// The current track on a glass card: artwork, title, uploader, scrubber, transport, loop,
/// volume, lyrics toggle and the ⋯ menu; "Not playing" with disabled controls when idle. With
/// lyrics toggled on, `LyricsView` fills the height below the card.
struct NowPlayingCard: View {
    let model: AppModel
    @Environment(\.lyricsFixture) private var lyricsFixture
    @ViewState private var showsLyrics = false
    /// The volume while its slider is dragged; committed on release.
    @ViewState private var volumeDrag: Double? = nil

    var body: some View {
        let track = model.store.currentTrack
        let lyricsShown = showsLyrics && track != nil
        VStack(spacing: 12) {
            card(track)
            if lyricsShown {
                LyricsView(model: model)
                    .transition(.opacity)
            }
        }
        .frame(maxHeight: lyricsShown ? .infinity : nil, alignment: .top)
        .layoutPriority(lyricsShown ? 1 : 0)
        .onAppear {
            if lyricsFixture != nil { showsLyrics = true }
        }
    }

    private func card(_ track: Track?) -> some View {
        let player = model.store.player
        return VStack(spacing: 14) {
            HStack(spacing: 12) {
                ArtworkView(path: track?.thumbPath, size: 64)
                VStack(alignment: .leading, spacing: 3) {
                    Text(track?.title ?? "Not playing")
                        .font(.headline)
                        .lineLimit(1)
                    if let uploader = track?.uploader {
                        Text(uploader)
                            .font(.subheadline)
                            .foregroundStyle(.secondary)
                            .lineLimit(1)
                    }
                }
                Spacer(minLength: 0)
                if let track, let player {
                    moreMenu(track, player)
                }
            }
            Scrubber(player: track == nil ? nil : player) { position in
                await model.seek(to: position)
            }
            HStack {
                loopButton(player?.loopMode ?? .none)
                    .disabled(player == nil)
                Spacer()
                TransportControls(model: model, isPlaying: player?.state == .playing)
                    .disabled(track == nil)
                Spacer()
                lyricsButton
                    .disabled(track == nil)
            }
            volumeRow(player)
        }
        .padding(16)
        .glassEffect(.regular, in: .rect(cornerRadius: 16))
    }

    private func loopButton(_ mode: LoopMode) -> some View {
        let symbol =
            switch mode {
            case .none, .all: "repeat"
            case .single: "repeat.1"
            case .shuffle: "shuffle"
            }
        return Button {
            Task { await model.cycleLoop() }
        } label: {
            Label("Loop", systemImage: symbol)
                .labelStyle(.iconOnly)
                .font(.body.weight(.semibold))
                .frame(width: 30, height: 30)
                .foregroundStyle(mode == .none ? AnyShapeStyle(.tertiary) : AnyShapeStyle(.tint))
                .contentShape(.rect)
        }
        .buttonStyle(.plain)
    }

    private var lyricsButton: some View {
        Button {
            withAnimation(.snappy(duration: 0.25)) { showsLyrics.toggle() }
        } label: {
            Label("Lyrics", systemImage: "quote.bubble")
                .labelStyle(.iconOnly)
                .font(.body.weight(.semibold))
                .frame(width: 30, height: 30)
                .foregroundStyle(showsLyrics ? AnyShapeStyle(.tint) : AnyShapeStyle(.secondary))
                .contentShape(.rect)
        }
        .buttonStyle(.plain)
    }

    private func volumeRow(_ player: PlayerState?) -> some View {
        HStack(spacing: 10) {
            Image(systemName: "speaker.wave.2.fill")
                .font(.callout)
                .foregroundStyle(.secondary)
                .frame(width: 18)
            Slider(
                value: Binding(
                    get: { volumeDrag ?? player?.volume ?? 0 },
                    set: { volumeDrag = $0 }),
                in: 0...100,
                onEditingChanged: { editing in
                    guard !editing, let value = volumeDrag else { return }
                    Task {
                        await model.setVolume(value)
                        volumeDrag = nil
                    }
                }
            )
            .controlSize(.small)
            .disabled(player == nil)
        }
    }

    private func moreMenu(_ track: Track, _ player: PlayerState) -> some View {
        Menu {
            if case .album(let albumID) = player.context, let album = model.store.album(id: albumID)
            {
                Button("Remove from Album…") {
                    model.confirm = ConfirmRequest(
                        title: "Remove “\(track.title)” from \(album.name)?",
                        message: "The song stays in your library.", actionTitle: "Remove"
                    ) {
                        await model.removeFromAlbum(track.id, album.id)
                    }
                }
            }
            Button("Delete from Library…") {
                let size = track.fileSize.map { $0.formatted(.byteCount(style: .file)) + " " }
                model.confirm = ConfirmRequest(
                    title: "Delete “\(track.title)” from your library?",
                    message: "The \(size ?? "")file moves to the Trash.", actionTitle: "Delete"
                ) {
                    await model.deleteTrack(track.id)
                }
            }
            Button("Show in Finder") {
                Task { await model.showInFinder(track.id) }
            }
            Button("Copy YouTube Link") {
                Task { await model.copyLink(track.id) }
            }
        } label: {
            Label("More", systemImage: "ellipsis.circle")
                .labelStyle(.iconOnly)
                .font(.title3)
                .foregroundStyle(.secondary)
        }
        .menuStyle(.button)
        .buttonStyle(.plain)
        .menuIndicator(.hidden)
        .fixedSize()
    }
}

/// The position slider with elapsed and remaining time. While playing, a `TimelineView` redraws
/// it once per second from the local clock; paused, stopped or idle there is no timer. Dragging
/// holds a local value that is sent as one `seek` on release.
private struct Scrubber: View {
    let player: PlayerState?
    let seek: @MainActor (Double) async -> Void
    @ViewState private var dragValue: Double? = nil

    var body: some View {
        if let player, player.state == .playing {
            TimelineView(.periodic(from: .now, by: 1)) { context in
                bar(PositionClock.position(player, now: context.date), player.duration)
            }
        } else {
            bar(player.map { PositionClock.position($0, now: .now) } ?? 0, player?.duration)
        }
    }

    private func bar(_ position: Double, _ duration: Double?) -> some View {
        let total = max(duration ?? 0, 0)
        let shown = min(dragValue ?? position, total)
        return VStack(spacing: 2) {
            Slider(
                value: Binding(get: { shown }, set: { dragValue = $0 }),
                in: 0...max(total, 1),
                onEditingChanged: { editing in
                    guard !editing, let value = dragValue else { return }
                    Task {
                        await seek(value)
                        dragValue = nil
                    }
                }
            )
            .controlSize(.small)
            .disabled(player == nil || duration == nil)
            HStack {
                Text(player == nil ? "--:--" : Self.time(shown))
                Spacer()
                Text(duration == nil ? "--:--" : "-" + Self.time(total - Double(Int(shown))))
            }
            .font(.caption.monospacedDigit())
            .foregroundStyle(.secondary)
        }
    }

    /// `m:ss`, or `h:mm:ss` from one hour.
    private static func time(_ seconds: Double) -> String {
        let total = max(0, Int(seconds))
        let (hours, minutes, secs) = (total / 3600, total / 60 % 60, total % 60)
        return hours > 0
            ? String(format: "%d:%02d:%02d", hours, minutes, secs)
            : String(format: "%d:%02d", minutes, secs)
    }
}
