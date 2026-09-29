import Foundation
import Testing

@testable import YplayerKit

private let videoID = "dQw4w9WgXcQ"

private func object(_ data: Data) throws -> NSDictionary {
    try #require(JSONSerialization.jsonObject(with: data) as? NSDictionary)
}

private func object(_ json: String) throws -> NSDictionary {
    try object(Data(json.utf8))
}

private func decodeEvent(_ json: String) throws -> Event {
    guard case .event(let event) = try Line.decode(Data(json.utf8)) else {
        Issue.record("not an event: \(json)")
        return .unknown("")
    }
    return event
}

private func decodeResponse(_ json: String) throws -> Response {
    guard case .response(let response) = try Line.decode(Data(json.utf8)) else {
        Issue.record("not a response: \(json)")
        throw ServiceError(code: "test", message: json)
    }
    return response
}

// Golden strings from `every_command_round_trips_with_exact_wire_json` in protocol.rs.
private let commandCases: [(String, Command)] = [
    (#"{"id":1,"cmd":"hello","protocol":1}"#, .hello(protocol: 1)),
    (#"{"id":1,"cmd":"subscribe"}"#, .subscribe),
    (#"{"id":1,"cmd":"library.get"}"#, .libraryGet),
    (#"{"id":1,"cmd":"now"}"#, .now),
    (
        #"{"id":1,"cmd":"add","url":"https://youtu.be/dQw4w9WgXcQ","album":{"id":3},"play":false}"#,
        .add(url: "https://youtu.be/dQw4w9WgXcQ", album: .id(3), play: false)
    ),
    (
        #"{"id":1,"cmd":"play","track_id":"dQw4w9WgXcQ","context":{"album_id":3}}"#,
        .play(trackID: videoID, context: .album(3))
    ),
    (#"{"id":1,"cmd":"pause"}"#, .pause),
    (#"{"id":1,"cmd":"resume"}"#, .resume),
    (#"{"id":1,"cmd":"toggle"}"#, .toggle),
    (#"{"id":1,"cmd":"stop"}"#, .stop),
    (#"{"id":1,"cmd":"next"}"#, .next),
    (#"{"id":1,"cmd":"prev"}"#, .prev),
    (#"{"id":1,"cmd":"seek","position":42.5}"#, .seek(position: 42.5)),
    (#"{"id":1,"cmd":"volume","value":55.5}"#, .volume(value: 55.5)),
    (#"{"id":1,"cmd":"loop","mode":"shuffle"}"#, .loop(mode: .shuffle)),
    (
        #"{"id":1,"cmd":"queue.play_next","track_id":"dQw4w9WgXcQ"}"#,
        .queuePlayNext(trackID: videoID)
    ),
    (#"{"id":1,"cmd":"album.create","name":"Zutomayo"}"#, .albumCreate(name: "Zutomayo")),
    (
        #"{"id":1,"cmd":"album.rename","album_id":3,"name":"ZTMY"}"#,
        .albumRename(albumID: 3, name: "ZTMY")
    ),
    (#"{"id":1,"cmd":"album.delete","album_id":3}"#, .albumDelete(albumID: 3)),
    (
        #"{"id":1,"cmd":"album.add","album_id":3,"track_id":"dQw4w9WgXcQ"}"#,
        .albumAdd(albumID: 3, trackID: videoID)
    ),
    (
        #"{"id":1,"cmd":"album.remove","album_id":3,"track_id":"dQw4w9WgXcQ"}"#,
        .albumRemove(albumID: 3, trackID: videoID)
    ),
    (
        #"{"id":1,"cmd":"album.reorder","album_id":3,"track_ids":["b","a"]}"#,
        .albumReorder(albumID: 3, trackIDs: ["b", "a"])
    ),
    (
        #"{"id":1,"cmd":"track.delete","track_id":"dQw4w9WgXcQ","to_trash":false}"#,
        .trackDelete(trackID: videoID, toTrash: false)
    ),
    (
        #"{"id":1,"cmd":"track.rename","track_id":"dQw4w9WgXcQ","title":"New"}"#,
        .trackRename(trackID: videoID, title: "New")
    ),
    (#"{"id":1,"cmd":"track.retry","track_id":"dQw4w9WgXcQ"}"#, .trackRetry(trackID: videoID)),
    (#"{"id":1,"cmd":"rescan"}"#, .rescan),
    (#"{"id":1,"cmd":"lyrics","track_id":"dQw4w9WgXcQ"}"#, .lyrics(trackID: videoID)),
    (#"{"id":1,"cmd":"queue.get"}"#, .queueGet),
    (
        #"{"id":1,"cmd":"queue.remove","section":"upcoming","index":2,"track_id":"dQw4w9WgXcQ"}"#,
        .queueRemove(section: .upcoming, index: 2, trackID: videoID)
    ),
    (
        #"{"id":1,"cmd":"queue.move","from":0,"to":2,"track_id":"dQw4w9WgXcQ"}"#,
        .queueMove(from: 0, to: 2, trackID: videoID)
    ),
    (#"{"id":1,"cmd":"queue.clear"}"#, .queueClear),
    (
        #"{"id":1,"cmd":"queue.jump","section":"next","index":1,"track_id":"dQw4w9WgXcQ"}"#,
        .queueJump(section: .next, index: 1, trackID: videoID)
    ),
]

@Test(arguments: commandCases)
func commandEncodesToGoldenJSON(_ wire: String, _ command: Command) throws {
    #expect(try object(command.line(id: 1)) == object(wire))
}

@Test func everyCommandHasAGoldenCase() {
    #expect(Set(commandCases.map { $0.1.name }).count == 32)
}

// Rust `add_defaults_play_true` / `track_delete_defaults_to_trash_true`: the Swift encoding
// of the defaulted command is the golden string with the defaults spelled out.
@Test func defaultedCommandsMatchRustDefaults() throws {
    let expected = try object(#"{"id":2,"cmd":"add","url":"u","album":null,"play":true}"#)
    for wire in [
        #"{"id":2,"cmd":"add","url":"u"}"#, #"{"id":2,"cmd":"add","url":"u","album":null}"#,
    ] {
        let golden = try #require(try object(wire).mutableCopy() as? NSMutableDictionary)
        if golden["album"] == nil { golden["album"] = NSNull() }
        golden["play"] = true
        #expect(golden == expected)
        #expect(try object(Command.add(url: "u", album: nil, play: true).line(id: 2)) == golden)
    }
    let golden = try #require(
        try object(#"{"id":2,"cmd":"track.delete","track_id":"x"}"#).mutableCopy()
            as? NSMutableDictionary)
    golden["to_trash"] = true
    #expect(try object(Command.trackDelete(trackID: "x", toTrash: true).line(id: 2)) == golden)
}

@Test func commandLineEndsWithSingleNewline() {
    let line = Command.trackRename(trackID: videoID, title: "a\nb").line(id: 1)
    #expect(line.last == UInt8(ascii: "\n"))
    #expect(line.filter { $0 == UInt8(ascii: "\n") }.count == 1)
}

private let goldenTrack = Track(
    id: videoID, title: videoID, uploader: nil, duration: nil, webpageURL: nil, audioPath: nil,
    format: nil, fileSize: nil, addedAt: 1, lastPlayed: nil, state: .downloading, thumbPath: nil)

// Golden strings from `events_serialize_to_exact_json` in protocol.rs.
private let eventCases: [(String, Event)] = [
    (
        #"{"event":"player","state":"playing","track_id":"dQw4w9WgXcQ","context":{"library":true},"position":12.5,"at_ms":1700000000000,"duration":200.0,"volume":80.0,"loop":"all"}"#,
        .player(
            PlayerState(
                state: .playing, trackID: videoID, context: .library, position: 12.5,
                atMs: 1_700_000_000_000, duration: 200.0, volume: 80.0, loopMode: .all))
    ),
    (
        #"{"event":"track.upsert","track":{"id":"dQw4w9WgXcQ","title":"dQw4w9WgXcQ","uploader":null,"duration":null,"webpage_url":null,"audio_path":null,"format":null,"file_size":null,"added_at":1,"last_played":null,"state":"downloading","thumb_path":null}}"#,
        .trackUpsert(goldenTrack)
    ),
    (#"{"event":"track.removed","track_id":"dQw4w9WgXcQ"}"#, .trackRemoved(videoID)),
    (
        #"{"event":"album.upsert","album":{"id":1,"name":"Inbox","track_ids":["a","b"],"created_at":5,"last_used_at":null}}"#,
        .albumUpsert(
            Album(id: 1, name: "Inbox", trackIDs: ["a", "b"], createdAt: 5, lastUsedAt: nil))
    ),
    (#"{"event":"album.removed","album_id":1}"#, .albumRemoved(1)),
    (
        #"{"event":"download","track_id":"dQw4w9WgXcQ","phase":"downloading","bytes":10,"total":null,"error":null}"#,
        .download(
            DownloadEvent(trackID: videoID, phase: .downloading, bytes: 10, total: nil, error: nil))
    ),
    (
        #"{"event":"toast","severity":"warn","message":"m"}"#,
        .toast(Toast(severity: .warn, message: "m"))
    ),
    (#"{"event":"resync"}"#, .resync),
    (
        #"{"event":"queue","next":["n"],"upcoming":["a","b"],"more":true,"context":{"album_id":3}}"#,
        .queue(QueueState(next: ["n"], upcoming: ["a", "b"], more: true, context: .album(3)))
    ),
    (
        #"{"event":"queue","next":[],"upcoming":[],"more":false,"context":null}"#,
        .queue(.empty)
    ),
]

@Test(arguments: eventCases)
func eventDecodesFromGoldenJSON(_ wire: String, _ event: Event) throws {
    #expect(try decodeEvent(wire) == event)
}

@Test func unknownEventNameDecodesAsUnknown() throws {
    #expect(
        try decodeEvent(#"{"event":"queue.changed","items":[1,2]}"#) == .unknown("queue.changed"))
}

// Golden values from `response_ok_omits_error_and_err_omits_result` in protocol.rs.
@Test func okResponseDecodesWithResult() throws {
    let response = try decodeResponse(#"{"id": 3, "ok": true, "result": {}}"#)
    #expect(response.id == 3)
    #expect(response.ok)
    #expect(response.error == nil)
    #expect(try response.result(as: EmptyResult.self) == EmptyResult())

    let hello = try decodeResponse(
        #"{"id":1,"ok":true,"result":{"protocol":1,"server_version":"0.1.0"}}"#)
    #expect(
        try hello.result(as: HelloResult.self) == HelloResult(protocol: 1, serverVersion: "0.1.0"))

    let synced = try decodeResponse(
        #"{"id":5,"ok":true,"result":{"synced":true,"lines":[{"t_ms":1500,"text":"秒針を噛む"}]}}"#)
    #expect(
        try synced.result(as: LyricsResult.self)
            == .lines(synced: true, [LyricLine(tMs: 1500, text: "秒針を噛む")]))

    let plain = try decodeResponse(
        #"{"id":6,"ok":true,"result":{"synced":false,"lines":[{"t_ms":null,"text":"a"}]}}"#)
    #expect(
        try plain.result(as: LyricsResult.self)
            == .lines(synced: false, [LyricLine(tMs: nil, text: "a")]))

    let missing = try decodeResponse(#"{"id":7,"ok":true,"result":{"missing":true}}"#)
    #expect(try missing.result(as: LyricsResult.self) == .missing)
}

@Test func queueGetResultDecodes() throws {
    let response = try decodeResponse(
        #"{"id":8,"ok":true,"result":{"next":["秒針を噛む"],"upcoming":["残機","猫リセット"],"more":false,"context":{"library":true}}}"#
    )
    #expect(
        try response.result(as: QueueState.self)
            == QueueState(
                next: ["秒針を噛む"], upcoming: ["残機", "猫リセット"], more: false, context: .library))
}

@Test func errorResponseDecodesCodeAndMessage() throws {
    let response = try decodeResponse(
        #"{"id": 4, "ok": false, "error": {"code": "not_found", "message": "no such track"}}"#)
    #expect(response.id == 4)
    #expect(!response.ok)
    let expected = ServiceError(code: "not_found", message: "no such track")
    #expect(response.error == expected)
    #expect(throws: expected) { try response.result(as: EmptyResult.self) }
}

@Test func trackWithAllNullOptionalsDecodes() throws {
    let json = #"""
        {"id":"x","title":"t","uploader":null,"duration":null,"webpage_url":null,"audio_path":null,
        "format":null,"file_size":null,"added_at":null,"last_played":null,"state":"complete",
        "thumb_path":null}
        """#
    let track = try JSONDecoder().decode(Track.self, from: Data(json.utf8))
    #expect(
        track
            == Track(
                id: "x", title: "t", uploader: nil, duration: nil, webpageURL: nil, audioPath: nil,
                format: nil, fileSize: nil, addedAt: nil, lastPlayed: nil, state: .complete,
                thumbPath: nil))
}

@Test func cjkTitlesRoundTrip() throws {
    let track = Track(
        id: videoID, title: "ずっと真夜中でいいのに。『秒針を噛む』MV", uploader: "ずっと真夜中でいいのに。",
        duration: 231, webpageURL: "https://www.youtube.com/watch?v=dQw4w9WgXcQ",
        audioPath: "/tmp/秒針を噛む [dQw4w9Wg]/audio.webm", format: "webm", fileSize: 4_000_000,
        addedAt: 1_700_000_000, lastPlayed: nil, state: .complete, thumbPath: nil)
    let back = try JSONDecoder().decode(Track.self, from: JSONEncoder().encode(track))
    #expect(back == track)

    let line = Command.trackRename(trackID: videoID, title: "아이유 좋은 날").line(id: 9)
    #expect(try object(line)["title"] as? String == "아이유 좋은 날")
}

// Rust `player_state_uses_loop_key`.
@Test func playerStateUsesLoopKey() throws {
    let state = PlayerState(
        state: .stopped, trackID: nil, context: nil, position: 0, atMs: 0, duration: nil,
        volume: 100, loopMode: .single)
    let data = try JSONEncoder().encode(state)
    let wire = try object(data)
    #expect(wire["loop"] as? String == "single")
    #expect(wire["loop_mode"] == nil)
    #expect(try JSONDecoder().decode(PlayerState.self, from: data) == state)
}

// Rust `context_and_album_refs_use_untagged_shapes`.
@Test func contextAndAlbumRefShapes() throws {
    let contexts: [(ContextRef, String)] = [
        (.album(3), #"{"album_id": 3}"#), (.library, #"{"library": true}"#),
    ]
    for (context, wire) in contexts {
        #expect(try object(JSONEncoder().encode(context)) == object(wire))
        #expect(try JSONDecoder().decode(ContextRef.self, from: Data(wire.utf8)) == context)
    }
    #expect(throws: DecodingError.self) {
        try JSONDecoder().decode(ContextRef.self, from: Data(#"{"library": false}"#.utf8))
    }

    let albums: [(AlbumRef, String)] = [
        (.id(3), #"{"id": 3}"#), (.name("Inbox"), #"{"name": "Inbox"}"#),
    ]
    for (album, wire) in albums {
        #expect(try object(JSONEncoder().encode(album)) == object(wire))
        #expect(try JSONDecoder().decode(AlbumRef.self, from: Data(wire.utf8)) == album)
    }
}
