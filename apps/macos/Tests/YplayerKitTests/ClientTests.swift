import Foundation
import Testing

@testable import YplayerKit

private let yplayBin = ProcessInfo.processInfo.environment["YPLAY_BIN"]

private struct TimedOut: Error, CustomStringConvertible {
    let what: String
    var description: String { "timed out waiting for \(what)" }
}

private func waitUntil(
    _ what: String, timeout: Duration = .seconds(10), _ done: () async -> Bool
) async throws {
    let deadline = ContinuousClock.now + timeout
    while !(await done()) {
        guard ContinuousClock.now < deadline else { throw TimedOut(what: what) }
        try await Task.sleep(for: .milliseconds(20))
    }
}

/// A `yplay serve` child with its own socket, cache and HOME (never the installed service).
private final class TestService {
    let socket: String
    private let home: URL
    private var process: Process?

    init(_ n: Int) throws {
        let pid = ProcessInfo.processInfo.processIdentifier
        socket = "/tmp/ypsw-\(pid)-\(n).sock"
        home = FileManager.default.temporaryDirectory.appending(path: "ypsw-\(pid)-\(n)")
        try? FileManager.default.removeItem(at: home)
        try FileManager.default.createDirectory(at: home, withIntermediateDirectories: true)
    }

    func start() async throws {
        let process = Process()
        process.executableURL = URL(filePath: try #require(yplayBin))
        process.arguments = [
            "serve", "--dir", home.appending(path: "cache").path(percentEncoded: false),
            "--socket", socket,
        ]
        var environment = ProcessInfo.processInfo.environment
        environment["HOME"] = home.path(percentEncoded: false)
        environment["YPLAY_NO_UPDATE"] = "1"
        environment["YPLAY_MPV_EXTRA_ARGS"] = "--ao=null"
        process.environment = environment
        process.standardOutput = FileHandle.nullDevice
        process.standardError = FileHandle.nullDevice
        try process.run()
        self.process = process
        let socket = socket
        try await waitUntil("the service socket") {
            FileManager.default.fileExists(atPath: socket)
        }
    }

    /// SIGTERM and wait for exit; the service removes its socket on the way out.
    func stop() async throws {
        guard let process else { return }
        process.terminate()
        try await waitUntil("the service to exit") { !process.isRunning }
        self.process = nil
    }

    /// Kills the service if still running and removes its socket and temp dirs.
    func cleanup() {
        if let process, process.isRunning {
            process.terminate()
            process.waitUntilExit()
        }
        try? FileManager.default.removeItem(atPath: socket)
        try? FileManager.default.removeItem(at: home)
    }
}

/// Everything a client yields on `updates`, in order.
private actor Recorder {
    private(set) var updates: [ClientUpdate] = []

    func append(_ update: ClientUpdate) {
        updates.append(update)
    }

    var connections: [SubscribeResult] {
        updates.compactMap {
            if case .connected(let result) = $0 { result } else { nil }
        }
    }

    var disconnects: [Duration] {
        updates.compactMap {
            if case .disconnected(let retry) = $0 { retry } else { nil }
        }
    }

    var events: [Event] {
        updates.compactMap {
            if case .event(let event) = $0 { event } else { nil }
        }
    }
}

private func record(_ client: ServiceClient) -> Recorder {
    let recorder = Recorder()
    Task {
        for await update in client.updates {
            await recorder.append(update)
        }
    }
    return recorder
}

/// Starts a client against `service` and waits for its first `.connected`.
private func connect(_ service: TestService) async throws -> (ServiceClient, Recorder) {
    let client = ServiceClient(socketPath: service.socket)
    let log = record(client)
    await client.start()
    try await waitUntil("connected") { await !log.connections.isEmpty }
    return (client, log)
}

@Suite(
    .enabled(
        if: yplayBin != nil,
        "YPLAY_BIN is unset: run `cargo build --release`, then scripts/swift-test.sh"))
struct ClientTests {
    @Test func connectedArrivesWithSubscribeResult() async throws {
        let service = try TestService(1)
        defer { service.cleanup() }
        try await service.start()
        let (_, log) = try await connect(service)
        let updates = await log.updates
        #expect(updates.first == .connecting)
        let subscribed = try #require(await log.connections.first)
        #expect(subscribed.player.state == .stopped)
        #expect(subscribed.player.trackID == nil)
    }

    @Test func libraryGetReturnsEmptyLibrary() async throws {
        let service = try TestService(2)
        defer { service.cleanup() }
        try await service.start()
        let (client, _) = try await connect(service)
        let snapshot = try await client.send(.libraryGet, as: LibrarySnapshot.self)
        #expect(snapshot.tracks.isEmpty)
        #expect(snapshot.albums.isEmpty)
    }

    @Test func albumCreateRepliesAndEmitsUpsert() async throws {
        let service = try TestService(3)
        defer { service.cleanup() }
        try await service.start()
        let (client, log) = try await connect(service)
        let created = try await client.send(
            .albumCreate(name: "ずっと真夜中でいいのに。"), as: AlbumCreateResult.self)
        #expect(created.album.name == "ずっと真夜中でいいのに。")
        #expect(created.album.trackIDs.isEmpty)
        try await waitUntil("album.upsert") {
            await log.events.contains(.albumUpsert(created.album))
        }
    }

    @Test func serverErrorMapsToServiceError() async throws {
        let service = try TestService(4)
        defer { service.cleanup() }
        try await service.start()
        let (client, _) = try await connect(service)
        let error = await #expect(throws: ServiceError.self) {
            try await client.send(.albumDelete(albumID: 9999))
        }
        #expect(error?.code == "not_found")
    }

    @Test func reconnectsAfterServiceRestartWithinBackoff() async throws {
        let service = try TestService(5)
        defer { service.cleanup() }
        try await service.start()
        let (_, log) = try await connect(service)

        try await service.stop()
        try await waitUntil("disconnected") { await !log.disconnects.isEmpty }
        #expect(await log.disconnects.first == .seconds(1))

        try await service.start()
        // The retry pending at this point is at most 2 s away (1 s, then 2 s).
        try await waitUntil("reconnected", timeout: .seconds(5)) {
            await log.connections.count == 2
        }
        let retries = await log.disconnects
        #expect(zip(retries, retries.dropFirst()).allSatisfy { $1 == $0 * 2 })

        // A successful connect resets the backoff to 1 s.
        try await service.stop()
        try await waitUntil("disconnected again") {
            await log.disconnects.count == retries.count + 1
        }
        #expect(await log.disconnects.last == .seconds(1))
    }

    @Test func requestWhileDisconnectedFailsFast() async throws {
        let service = try TestService(6)
        defer { service.cleanup() }
        try await service.start()
        let (client, log) = try await connect(service)
        try await service.stop()
        try await waitUntil("disconnected") { await !log.disconnects.isEmpty }

        let started = ContinuousClock.now
        let error = await #expect(throws: ServiceError.self) {
            try await client.send(.now, as: NowResult.self)
        }
        #expect(error?.code == "not_connected")
        #expect(ContinuousClock.now - started < .milliseconds(100))
    }

    @Test func clientNeverLeaksTheSocketFile() async throws {
        let service = try TestService(7)
        defer { service.cleanup() }
        try await service.start()
        let (_, log) = try await connect(service)
        try await service.stop()
        #expect(!FileManager.default.fileExists(atPath: service.socket))
        // At least one reconnect attempt against the missing path.
        try await waitUntil("a failed reconnect") { await log.disconnects.count >= 2 }
        #expect(!FileManager.default.fileExists(atPath: service.socket))
    }
}
