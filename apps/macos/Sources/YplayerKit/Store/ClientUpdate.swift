/// What `ServiceClient.updates` yields: connection changes and service events.
public enum ClientUpdate: Sendable {
    case connecting
    case connected(SubscribeResult)
    case disconnected(retryIn: Duration)
    case event(Event)
}
