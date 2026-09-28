import Foundation

/// What the client reports on `ServiceClient.updates`.
public enum ClientUpdate: Equatable, Sendable {
    case connecting
    case connected(SubscribeResult)
    case disconnected(retryIn: Duration)
    case event(Event)
}

/// The app's connection to `yplay serve`: handshake, subscription, requests, reconnect.
public actor ServiceClient {
    private static let protocolVersion = 1
    private static let requestTimeout: Duration = .seconds(10)
    private static let maxBackoff: Duration = .seconds(30)
    private static let mismatchRetry: Duration = .seconds(30)

    public nonisolated let updates: AsyncStream<ClientUpdate>
    private let output: AsyncStream<ClientUpdate>.Continuation
    private let socketPath: String
    private var loop: Task<Void, Never>?
    /// The open (or opening) connection.
    private var socket: UnixSocket?
    /// True once `hello` and `subscribe` succeeded on `socket`.
    private var ready = false
    /// Events read after the `subscribe` reply but before `.connected` was yielded.
    private var early: [Event] = []
    private var backoff: Duration = .seconds(1)
    private var nextID: UInt64 = 0
    private var pending: [UInt64: Pending] = [:]

    private struct Pending {
        let continuation: CheckedContinuation<Response, any Error>
        let timeout: Task<Void, Never>
    }

    public init(socketPath: String) {
        self.socketPath = socketPath
        (updates, output) = AsyncStream.makeStream(of: ClientUpdate.self)
    }

    /// `YPLAY_SOCKET`, else `~/Library/Application Support/yplayer/yplay.sock`.
    public static func defaultSocketPath() -> String {
        if let path = ProcessInfo.processInfo.environment["YPLAY_SOCKET"], !path.isEmpty {
            return path
        }
        return URL.applicationSupportDirectory.appending(path: "yplayer/yplay.sock")
            .path(percentEncoded: false)
    }

    /// Connects (and keeps reconnecting); while waiting to retry, connects immediately.
    public func start() {
        guard socket == nil else { return }
        loop?.cancel()
        loop = Task { await run() }
    }

    public func send<R: Decodable & Sendable>(_ cmd: Command, as type: R.Type) async throws -> R {
        guard ready, let socket else { throw Self.notConnected }
        return try await request(cmd, on: socket).result(as: type)
    }

    public func send(_ cmd: Command) async throws {
        guard ready, let socket else { throw Self.notConnected }
        if let error = try await request(cmd, on: socket).error {
            throw error
        }
    }

    private static var notConnected: ServiceError {
        ServiceError(code: "not_connected", message: "not connected to the yplay service")
    }

    private func run() async {
        while !Task.isCancelled {
            let retry = await session()
            output.yield(.disconnected(retryIn: retry))
            do {
                try await Task.sleep(for: retry)
            } catch {
                return
            }
        }
    }

    /// One connection from connect to end of stream; returns the delay before the next attempt.
    private func session() async -> Duration {
        output.yield(.connecting)
        let socket = UnixSocket(path: socketPath)
        self.socket = socket
        do {
            try await socket.open()
        } catch {
            close(socket)
            return nextBackoff()
        }
        let reader = Task { await read(socket) }
        do {
            let hello = try await request(.hello(protocol: Self.protocolVersion), on: socket)
                .result(as: HelloResult.self)
            guard hello.protocol == Self.protocolVersion else {
                throw ServiceError(
                    code: "protocol_mismatch", message: "service speaks protocol \(hello.protocol)")
            }
            let subscribed = try await request(.subscribe, on: socket)
                .result(as: SubscribeResult.self)
            guard self.socket === socket else { throw Self.notConnected }
            ready = true
            backoff = .seconds(1)
            output.yield(.connected(subscribed))
            for event in early {
                output.yield(.event(event))
            }
            early = []
        } catch {
            close(socket)
            await reader.value
            if let error = error as? ServiceError, error.code == "protocol_mismatch" {
                return Self.mismatchRetry
            }
            return nextBackoff()
        }
        await reader.value
        return nextBackoff()
    }

    private func nextBackoff() -> Duration {
        let delay = backoff
        backoff = min(backoff * 2, Self.maxBackoff)
        return delay
    }

    /// Reads lines until end of stream, an error, or an over-long line, then closes `socket`.
    private func read(_ socket: UnixSocket) async {
        var framer = LineFramer()
        while self.socket === socket {
            guard let chunk = try? await socket.receive(), self.socket === socket,
                let lines = framer.append(chunk)
            else { break }
            for line in lines {
                dispatch(line)
            }
        }
        close(socket)
    }

    private func dispatch(_ line: Data) {
        switch try? Line.decode(line) {
        case .response(let response):
            guard let request = pending.removeValue(forKey: response.id) else { return }
            request.timeout.cancel()
            request.continuation.resume(returning: response)
        case .event(let event):
            if ready {
                output.yield(.event(event))
            } else {
                early.append(event)
            }
        case nil:
            break
        }
    }

    private func request(_ cmd: Command, on socket: UnixSocket) async throws -> Response {
        nextID += 1
        let id = nextID
        return try await withCheckedThrowingContinuation { continuation in
            let timeout = Task {
                do {
                    try await Task.sleep(for: Self.requestTimeout)
                } catch {
                    return
                }
                expire(id)
            }
            pending[id] = Pending(continuation: continuation, timeout: timeout)
            socket.send(cmd.line(id: id))
        }
    }

    private func expire(_ id: UInt64) {
        pending.removeValue(forKey: id)?.continuation.resume(
            throwing: ServiceError(code: "timeout", message: "the yplay service did not reply"))
    }

    /// Closes `socket`; if it is the current connection, fails its outstanding requests.
    private func close(_ socket: UnixSocket) {
        socket.close()
        guard self.socket === socket else { return }
        self.socket = nil
        ready = false
        early = []
        let failed = pending
        pending = [:]
        for request in failed.values {
            request.timeout.cancel()
            request.continuation.resume(throwing: Self.notConnected)
        }
    }
}
