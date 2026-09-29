import Foundation

public enum ConnectionStatus: Equatable, Sendable {
    case connecting
    case connected
    case disconnected(retryAt: Date)
}

public struct DownloadProgress: Equatable, Sendable {
    public var phase: DownloadPhase
    /// `bytes / total` when both are known.
    public var fraction: Double?

    public init(phase: DownloadPhase, fraction: Double?) {
        self.phase = phase
        self.fraction = fraction
    }
}

public struct ToastItem: Equatable, Identifiable, Sendable {
    public let id: UUID
    public var severity: Severity
    public var message: String
    public let createdAt: Date

    public init(
        id: UUID = UUID(), severity: Severity, message: String, createdAt: Date = .now
    ) {
        self.id = id
        self.severity = severity
        self.message = message
        self.createdAt = createdAt
    }
}
