// KeychainKeyStoreTests (M5.3) — exercises the real iOS Keychain on the
// simulator, the iOS analogue of Android's keystore coverage. Verifies the
// core's `KeyStore` contract that session persistence (M1.3) relies on:
// put/get round-trip, overwrite-replaces, get-absent → nil, delete, and
// delete-absent is a no-op (not an error).
//
// Each test uses a unique service namespace and tears its items down, so runs
// don't collide and nothing leaks into the shared login keychain.
import XCTest
@testable import Pigeon

final class KeychainKeyStoreTests: XCTestCase {
    private var service = ""
    private var store: KeychainKeyStore!

    override func setUp() {
        super.setUp()
        service = "com.pigeon.mobile.test.\(UUID().uuidString)"
        store = KeychainKeyStore(service: service)
    }

    override func tearDown() {
        // Best-effort cleanup of anything a test wrote.
        for key in ["session", "mls", "k"] { try? store.delete(key: key) }
        store = nil
        super.tearDown()
    }

    func testPutThenGetRoundTrips() throws {
        let value = Data("pigeon.session.v1 blob".utf8)
        try store.put(key: "session", value: value)
        XCTAssertEqual(try store.get(key: "session"), value)
    }

    func testGetAbsentReturnsNil() throws {
        XCTAssertNil(try store.get(key: "never-written"))
    }

    func testPutReplacesExistingValue() throws {
        try store.put(key: "k", value: Data("first".utf8))
        try store.put(key: "k", value: Data("second".utf8))
        XCTAssertEqual(try store.get(key: "k"), Data("second".utf8))
    }

    func testDeleteRemovesValue() throws {
        try store.put(key: "k", value: Data([0x01, 0x02, 0x03]))
        try store.delete(key: "k")
        XCTAssertNil(try store.get(key: "k"))
    }

    func testDeleteAbsentIsNoOp() throws {
        // Must not throw — matches the core's contract (delete of a missing key
        // is a no-op) and Android's `remove`.
        XCTAssertNoThrow(try store.delete(key: "never-written"))
    }

    func testEmptyValueRoundTrips() throws {
        try store.put(key: "k", value: Data())
        XCTAssertEqual(try store.get(key: "k"), Data())
    }

    func testBinaryValueRoundTrips() throws {
        // The stored blobs are opaque bytes (a token blob, MLS state) — verify a
        // non-UTF-8 payload survives intact.
        let value = Data((0..<256).map { UInt8($0 & 0xff) })
        try store.put(key: "mls", value: value)
        XCTAssertEqual(try store.get(key: "mls"), value)
    }
}
