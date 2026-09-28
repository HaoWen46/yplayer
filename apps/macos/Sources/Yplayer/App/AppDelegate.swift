import AppKit
import YplayerKit

/// Debug flags: `--snapshot-dir <dir>` renders the debug states to PNGs and exits;
/// `--open-popover` opens the popover 1 s after launch.
struct LaunchOptions {
    var snapshotDir: URL?
    var openPopover = false

    init(arguments: [String]) {
        var rest = arguments.dropFirst().makeIterator()
        while let argument = rest.next() {
            switch argument {
            case "--snapshot-dir":
                snapshotDir = rest.next().map { URL(filePath: $0) }
            case "--open-popover":
                openPopover = true
            default:
                break
            }
        }
    }
}

@MainActor
final class AppDelegate: NSObject, NSApplicationDelegate {
    private let options: LaunchOptions
    private var statusItem: StatusItemController?
    private var nowPlaying: NowPlayingController?

    init(options: LaunchOptions) {
        self.options = options
    }

    func applicationDidFinishLaunching(_ notification: Notification) {
        if let dir = options.snapshotDir {
            Task {
                do {
                    try await SnapshotRenderer.render(to: dir)
                    exit(0)
                } catch {
                    FileHandle.standardError.write(Data("snapshot failed: \(error)\n".utf8))
                    exit(1)
                }
            }
            return
        }
        let model = AppModel(
            client: ServiceClient(socketPath: ServiceClient.defaultSocketPath()),
            store: LibraryStore())
        model.start()
        nowPlaying = NowPlayingController(model: model)
        let statusItem = StatusItemController(model: model)
        self.statusItem = statusItem
        if options.openPopover {
            Task {
                try? await Task.sleep(for: .seconds(1))
                statusItem.show()
            }
        }
    }
}
