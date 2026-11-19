// CleartextPolicyTests — `cleartextRefusal`, with the platform policy injected.
// Mirrors Android's CleartextPolicyTest.kt: same cases, same expected wording.
import XCTest
@testable import Pigeon

final class CleartextPolicyTests: XCTestCase {

    private let onlyDevHost: (String) -> Bool = { $0 == "localhost" }
    private let never: (String) -> Bool = { host in
        XCTFail("the policy must not be consulted for \(host)"); return false
    }

    func testHttpsIsAlwaysAllowedWithoutConsultingThePolicy() {
        XCTAssertNil(cleartextRefusal("https://pigeon.example", isCleartextPermitted: never))
    }

    func testHttpToAPermittedHostIsAllowed() {
        XCTAssertNil(cleartextRefusal("http://localhost:8008", isCleartextPermitted: onlyDevHost))
    }

    func testHttpToAnyOtherHostIsRefused() {
        XCTAssertEqual(cleartextRefusal("http://pigeon.example", isCleartextPermitted: onlyDevHost),
                       cleartextRefused)
    }

    func testTheSchemeCheckIgnoresCaseAndSurroundingSpace() {
        XCTAssertEqual(cleartextRefusal("  HTTP://pigeon.example  ", isCleartextPermitted: onlyDevHost),
                       cleartextRefused)
    }

    func testAnUnparseableAddressIsLeftForTheCoreToReject() {
        XCTAssertNil(cleartextRefusal("not a url", isCleartextPermitted: never))
    }

    // iOS-only: this test bundle builds Debug, where the allowlist is exactly localhost.
    func testTheDebugAllowlistIsLocalhostOnly() {
        XCTAssertTrue(platformPermitsCleartext("localhost"))
        XCTAssertFalse(platformPermitsCleartext("pigeon.example"))
        XCTAssertFalse(platformPermitsCleartext("10.0.2.2"))
    }

    func testWordingMatchesAndroid() {
        XCTAssertEqual(cleartextRefused,
                       "This address uses http://, which would send your password and messages unencrypted. "
                       + "Use an https:// address.")
    }
}
