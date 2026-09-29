import Foundation

public struct ServiceError: Error, Codable, Equatable, Sendable {
    public var code: String
    public var message: String

    public init(code: String, message: String) {
        self.code = code
        self.message = message
    }
}

/// A reply line: `{id, ok, result?, error?: {code, message}}`.
public struct Response: Equatable, Sendable {
    public let id: UInt64
    public let ok: Bool
    public let error: ServiceError?
    /// The whole reply line; `result` is decoded from it on demand.
    let line: Data

    /// Decodes `result` as `R`; throws `error` when the reply is not ok.
    public func result<R: Decodable>(as type: R.Type) throws -> R {
        if let error { throw error }
        return try JSONDecoder().decode(ResultBox<R>.self, from: line).result
    }
}

private struct ResultBox<R: Decodable>: Decodable {
    let result: R
}

/// One line read from the socket.
public enum Line: Equatable, Sendable {
    case response(Response)
    case event(Event)

    public static func decode(_ line: Data) throws -> Line {
        switch try JSONDecoder().decode(Wire.self, from: line) {
        case .event(let event):
            return .event(event)
        case .response(let id, let ok, let error):
            return .response(Response(id: id, ok: ok, error: error, line: line))
        }
    }
}

private enum Wire: Decodable {
    case event(Event)
    case response(id: UInt64, ok: Bool, error: ServiceError?)

    private enum CodingKeys: String, CodingKey {
        case event, id, ok, error
    }

    init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        if container.contains(.event) {
            self = .event(try Event(from: decoder))
        } else {
            self = .response(
                id: try container.decode(UInt64.self, forKey: .id),
                ok: try container.decode(Bool.self, forKey: .ok),
                error: try container.decodeIfPresent(ServiceError.self, forKey: .error))
        }
    }
}
