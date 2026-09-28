import Foundation
import Network

/// One stream connection to a Unix domain socket (Network.framework).
final class UnixSocket: Sendable {
    private static let queue = DispatchQueue(label: "com.yplayer.service-client")
    private let connection: NWConnection

    init(path: String) {
        connection = NWConnection(to: .unix(path: path), using: .tcp)
    }

    /// Returns once the connection is ready; throws when the socket is missing or refuses.
    func open() async throws {
        let (states, sink) = AsyncStream.makeStream(of: NWConnection.State.self)
        connection.stateUpdateHandler = { sink.yield($0) }
        connection.start(queue: Self.queue)
        defer { connection.stateUpdateHandler = nil }
        for await state in states {
            switch state {
            case .ready:
                return
            case .waiting(let error), .failed(let error):
                connection.cancel()
                throw error
            case .cancelled:
                throw CancellationError()
            default:
                continue
            }
        }
        throw CancellationError()
    }

    /// The next chunk of bytes; nil at end of stream.
    func receive() async throws -> Data? {
        try await withCheckedThrowingContinuation { continuation in
            connection.receive(minimumIncompleteLength: 1, maximumLength: 64 * 1024) {
                data, _, isComplete, error in
                if let data, !data.isEmpty {
                    continuation.resume(returning: data)
                } else if let error {
                    continuation.resume(throwing: error)
                } else if isComplete {
                    continuation.resume(returning: nil)
                } else {
                    continuation.resume(returning: Data())
                }
            }
        }
    }

    func send(_ data: Data) {
        connection.send(content: data, completion: .contentProcessed { _ in })
    }

    func close() {
        connection.cancel()
    }
}

/// Splits a byte stream into `\n`-terminated lines.
struct LineFramer {
    /// Longest accepted line, without its `\n` (the service's `MAX_LINE`).
    static let maxLine = 1 << 20
    private var buffer = Data()

    /// Appends `chunk` and returns the complete lines (without `\n`); nil when a line
    /// exceeds `maxLine`.
    mutating func append(_ chunk: Data) -> [Data]? {
        buffer.append(chunk)
        var lines: [Data] = []
        var start = buffer.startIndex
        while let newline = buffer[start...].firstIndex(of: UInt8(ascii: "\n")) {
            guard newline - start <= Self.maxLine else { return nil }
            lines.append(Data(buffer[start..<newline]))
            start = newline + 1
        }
        buffer.removeSubrange(buffer.startIndex..<start)
        guard buffer.count <= Self.maxLine else { return nil }
        return lines
    }
}
