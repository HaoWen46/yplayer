import AppKit
import SwiftUI
import YplayerKit

/// The settings window's grouped form: Playback (loudness leveling), Music folder (Show in
/// Finder, Move…) and YouTube search (the API key), above the app's version. Controls are
/// disabled until the service's settings arrive (with the connection state shown while not
/// connected); errors show in red under their row.
struct SettingsView: View {
    let model: AppModel
    /// Asks for the folder to move the music folder into; nil when cancelled.
    let chooseDestination: @MainActor () async -> URL?
    @ViewState private var apiKey = ""
    @ViewState private var apiKeyError: String?
    @ViewState private var playbackError: String?
    /// The move target waiting for confirmation.
    @ViewState private var proposedMove: String?

    var body: some View {
        let settings = model.settings
        VStack(spacing: 0) {
            Form {
                if settings == nil, model.store.connection != .connected {
                    connection
                }
                playback(settings)
                musicFolder(settings)
                youTubeSearch(settings)
            }
            .formStyle(.grouped)
            .scrollDisabled(true)
            Text(Self.version)
                .font(.footnote)
                .foregroundStyle(.secondary)
                .padding(.bottom, 20)
        }
        .frame(width: 460)
        .fixedSize(horizontal: false, vertical: true)
        .onChange(of: settings?.apiKey, initial: true) { _, key in apiKey = key ?? "" }
        .alert(
            proposedMove.map { "Move your music to \(MusicFolderPath.display($0))?" } ?? "",
            isPresented: Binding(
                get: { proposedMove != nil }, set: { if !$0 { proposedMove = nil } }),
            presenting: proposedMove
        ) { target in
            Button("Move") {
                Task { await model.moveMusicFolder(to: target) }
            }
            .keyboardShortcut(.defaultAction)
            Button("Cancel", role: .cancel) {}
        } message: { _ in
            Text(
                "Playback stops for a moment while Yplayer moves the folder. The new place must "
                    + "be on the same disk.")
        }
    }

    /// Shown while there are no settings and no connection: connecting, or the service can't be
    /// reached.
    private var connection: some View {
        Section {
            if model.store.connection == .connecting {
                HStack(spacing: 8) {
                    ProgressView()
                        .controlSize(.small)
                    Text("Connecting to the yplay service…")
                }
            } else {
                Label {
                    Text("Can't reach the yplay service.")
                } icon: {
                    Image(systemName: "exclamationmark.triangle.fill")
                        .foregroundStyle(.orange)
                }
            }
        }
    }

    private func playback(_ settings: ServiceSettings?) -> some View {
        Section("Playback") {
            Toggle(
                isOn: Binding(
                    get: { settings?.levelLoudness ?? false }, set: { setLevelLoudness($0) })
            ) {
                Text("Even out loudness")
                Text(
                    "Plays every song at a similar volume. Each song is measured once, in the "
                        + "background, after it downloads.")
            }
            .toggleStyle(.switch)
            .disabled(settings?.loudnessAvailable != true)
            if settings?.loudnessAvailable == false {
                Label {
                    Text("Needs ffmpeg: \(Text("brew install ffmpeg").monospaced())")
                        .textSelection(.enabled)
                } icon: {
                    Image(systemName: "exclamationmark.triangle.fill")
                        .foregroundStyle(.orange)
                }
                .foregroundStyle(.secondary)
            }
            if let playbackError {
                Self.error(playbackError)
            }
        }
    }

    /// The folder's path with Show in Finder and Move…; a path too long for one row gets its own
    /// line above the buttons. A move shows "Moving…" and then clears, or shows its error.
    private func musicFolder(_ settings: ServiceSettings?) -> some View {
        let moving = if case .moving = model.folderMove { true } else { false }
        let path = Label {
            Text(settings.map { MusicFolderPath.display($0.musicFolder) } ?? "—")
                .lineLimit(1)
                .truncationMode(.middle)
                .help(settings?.musicFolder ?? "")
        } icon: {
            Image(systemName: "folder.fill")
                .foregroundStyle(.tint)
        }
        let buttons = HStack(spacing: 8) {
            Button("Show in Finder") {
                guard let folder = settings?.musicFolder else { return }
                NSWorkspace.shared.activateFileViewerSelecting([URL(filePath: folder)])
            }
            Button("Move…") {
                guard let folder = settings?.musicFolder else { return }
                chooseMove(of: folder)
            }
        }
        .disabled(settings == nil || moving)
        return Section("Music folder") {
            VStack(alignment: .leading, spacing: 8) {
                ViewThatFits(in: .horizontal) {
                    HStack(spacing: 8) {
                        path
                        Spacer(minLength: 8)
                        buttons
                    }
                    VStack(alignment: .trailing, spacing: 8) {
                        path
                            .frame(maxWidth: .infinity, alignment: .leading)
                        buttons
                    }
                }
                switch model.folderMove {
                case .moving:
                    HStack(spacing: 6) {
                        ProgressView()
                            .controlSize(.small)
                        Text("Moving…")
                            .foregroundStyle(.secondary)
                    }
                case .failed(let message):
                    Self.error(message)
                case nil:
                    EmptyView()
                }
            }
        }
    }

    private func youTubeSearch(_ settings: ServiceSettings?) -> some View {
        let key = apiKey.trimmingCharacters(in: .whitespacesAndNewlines)
        return Section("YouTube search") {
            VStack(alignment: .leading, spacing: 4) {
                HStack(spacing: 8) {
                    Text("API key")
                    SecureField("API key", text: $apiKey, prompt: Text("Not set"))
                        .labelsHidden()
                        .textFieldStyle(.roundedBorder)
                        .onSubmit { save(key) }
                    Button("Save") { save(key) }
                        .disabled(key.isEmpty || key == settings?.apiKey)
                    Button("Remove") { remove() }
                        .disabled(settings?.apiKey == nil)
                }
                Text("Only the yplay search command uses this.")
                    .font(.subheadline)
                    .foregroundStyle(.secondary)
            }
            .disabled(settings == nil)
            if let apiKeyError {
                Self.error(apiKeyError)
            }
        }
    }

    /// Applies the toggle at once; restores it when the service refuses.
    private func setLevelLoudness(_ on: Bool) {
        model.settings?.levelLoudness = on
        Task {
            playbackError = await model.changeSettings(levelLoudness: on)
            if playbackError != nil { model.settings?.levelLoudness = !on }
        }
    }

    private func save(_ key: String) {
        guard !key.isEmpty else { return }
        Task { apiKeyError = await model.changeSettings(apiKey: key) }
    }

    private func remove() {
        Task { apiKeyError = await model.changeSettings(apiKey: .some(nil)) }
    }

    /// Asks for a destination, then confirms moving `folder` into it (keeping its name).
    private func chooseMove(of folder: String) {
        Task {
            guard let parent = await chooseDestination() else { return }
            proposedMove = MusicFolderPath.moveTarget(
                for: folder, into: parent.path(percentEncoded: false))
        }
    }

    private static func error(_ message: String) -> some View {
        Text(message)
            .font(.callout)
            .foregroundStyle(.red)
            .fixedSize(horizontal: false, vertical: true)
    }

    /// "Yplayer <CFBundleShortVersionString>".
    private static var version: String {
        let short = Bundle.main.object(forInfoDictionaryKey: "CFBundleShortVersionString")
        return ["Yplayer", short as? String].compactMap { $0 }.joined(separator: " ")
    }
}
