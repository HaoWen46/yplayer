import Foundation
import Testing

@testable import YplayerKit

private func object(_ data: Data) throws -> NSDictionary {
    try #require(JSONSerialization.jsonObject(with: data) as? NSDictionary)
}

private func object(_ json: String) throws -> NSDictionary {
    try object(Data(json.utf8))
}

// The shared protocol contract in docs/plans/2026-09-29-polish-plan.md.
private let settingsCommandCases: [(String, Command)] = [
    (#"{"id":1,"cmd":"settings.get"}"#, .settingsGet),
    (#"{"id":1,"cmd":"settings.set"}"#, .settingsSet()),
    (#"{"id":1,"cmd":"settings.set","level_loudness":false}"#, .settingsSet(levelLoudness: false)),
    (#"{"id":1,"cmd":"settings.set","api_key":"AIza-key_1"}"#, .settingsSet(apiKey: "AIza-key_1")),
    (#"{"id":1,"cmd":"settings.set","api_key":null}"#, .settingsSet(apiKey: .some(nil))),
    (
        #"{"id":1,"cmd":"settings.set","level_loudness":true,"api_key":null}"#,
        .settingsSet(levelLoudness: true, apiKey: .some(nil))
    ),
    (
        #"{"id":1,"cmd":"library.move","to":"/Volumes/音楽/yt-audio"}"#,
        .libraryMove(to: "/Volumes/音楽/yt-audio")
    ),
]

@Test(arguments: settingsCommandCases)
func settingsCommandEncodesToContractJSON(_ wire: String, _ command: Command) throws {
    #expect(try object(command.line(id: 1)) == object(wire))
}

@Test func settingsSetOmitsFieldsNotGiven() throws {
    let loudness = try object(Command.settingsSet(levelLoudness: true).line(id: 1))
    #expect(loudness["api_key"] == nil)
    let key = try object(Command.settingsSet(apiKey: "k").line(id: 1))
    #expect(key["level_loudness"] == nil)
    let remove = try object(Command.settingsSet(apiKey: .some(nil)).line(id: 1))
    #expect(remove["api_key"] is NSNull)
}

@Test func settingsResultDecodes() throws {
    let json = #"""
        {"id":2,"ok":true,"result":{"level_loudness":true,"loudness_available":false,
        "api_key":null,"music_folder":"/Users/me/Music/yt-audio"}}
        """#
    guard case .response(let response) = try Line.decode(Data(json.utf8)) else {
        Issue.record("not a response")
        return
    }
    #expect(
        try response.result(as: ServiceSettings.self)
            == ServiceSettings(
                levelLoudness: true, loudnessAvailable: false, apiKey: nil,
                musicFolder: "/Users/me/Music/yt-audio"))
}

@Test func settingsEventDecodes() throws {
    let json = #"""
        {"event":"settings","level_loudness":false,"loudness_available":true,
        "api_key":"AIza-key_1","music_folder":"/Users/me/音楽/yt-audio"}
        """#
    guard case .event(let event) = try Line.decode(Data(json.utf8)) else {
        Issue.record("not an event")
        return
    }
    #expect(
        event
            == .settings(
                ServiceSettings(
                    levelLoudness: false, loudnessAvailable: true, apiKey: "AIza-key_1",
                    musicFolder: "/Users/me/音楽/yt-audio")))
}

@Test func libraryMoveResultDecodes() throws {
    let json = #"{"id":3,"ok":true,"result":{"restarting":true}}"#
    guard case .response(let response) = try Line.decode(Data(json.utf8)) else {
        Issue.record("not a response")
        return
    }
    #expect(try response.result(as: LibraryMoveResult.self) == LibraryMoveResult(restarting: true))
}

@Test func libraryMoveErrorDecodes() throws {
    let json = #"""
        {"id":4,"ok":false,"error":{"code":"conflict",
        "message":"Wait for downloads to finish before moving your music folder."}}
        """#
    guard case .response(let response) = try Line.decode(Data(json.utf8)) else {
        Issue.record("not a response")
        return
    }
    let expected = ServiceError(
        code: "conflict", message: "Wait for downloads to finish before moving your music folder.")
    #expect(throws: expected) { try response.result(as: LibraryMoveResult.self) }
}

@Test func moveTargetKeepsTheFolderName() {
    let folder = "/Users/me/Music/yt-audio"
    #expect(
        MusicFolderPath.moveTarget(for: folder, into: "/Volumes/Music") == "/Volumes/Music/yt-audio"
    )
    #expect(
        MusicFolderPath.moveTarget(for: folder, into: "/Volumes/Music/")
            == "/Volumes/Music/yt-audio")
    #expect(
        MusicFolderPath.moveTarget(for: folder + "/", into: "/Users/me") == "/Users/me/yt-audio")
    #expect(MusicFolderPath.moveTarget(for: folder, into: "/") == "/yt-audio")
    #expect(
        MusicFolderPath.moveTarget(for: "/Users/me/音楽/ずとまよ", into: "/Users/me/ミュージック")
            == "/Users/me/ミュージック/ずとまよ")
    #expect(
        MusicFolderPath.moveTarget(for: folder, into: "/Users/me/My Music")
            == "/Users/me/My Music/yt-audio")
}

@Test func homeFolderShowsAsTilde() {
    let home = "/Users/me"
    #expect(MusicFolderPath.display("/Users/me/Music/yt-audio", home: home) == "~/Music/yt-audio")
    #expect(MusicFolderPath.display("/Users/me", home: home) == "~")
    #expect(MusicFolderPath.display("/Users/me/Music", home: home + "/") == "~/Music")
    #expect(MusicFolderPath.display("/Users/me/音楽/ずとまよ", home: home) == "~/音楽/ずとまよ")
    #expect(MusicFolderPath.display("/Users/meow/Music", home: home) == "/Users/meow/Music")
    #expect(
        MusicFolderPath.display("/Volumes/Music/yt-audio", home: home) == "/Volumes/Music/yt-audio")
    #expect(MusicFolderPath.display("/tmp/Users/me/Music", home: home) == "/tmp/Users/me/Music")
}
