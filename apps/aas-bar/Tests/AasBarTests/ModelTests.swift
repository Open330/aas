import XCTest
@testable import AasBar

final class ModelTests: XCTestCase {
    func testUsageJSONContractAcceptsNotesAndMeters() throws {
        let json = #"{"accounts":[{"provider":"grok","name":"work","email":null,"active":true,"plan":"team","planLabel":"TEAM","headline":"Grok team","error":null,"notes":["rate remaining req=3 tok=9"],"meters":[{"label":"credits","usedPct":25.0,"resetMs":null}]}]}"#
        let decoded = try JSONDecoder().decode(UsageResponse.self, from: Data(json.utf8))

        XCTAssertEqual(decoded.accounts.count, 1)
        XCTAssertEqual(decoded.accounts[0].id, "grok/work")
        XCTAssertEqual(decoded.accounts[0].meters[0].remaining, 75)
        XCTAssertEqual(decoded.accounts[0].notes, ["rate remaining req=3 tok=9"])
    }

    func testSummaryMakesErrorsVisibleEvenWithoutMeters() {
        let account = Account(
            provider: "grok",
            name: "revoked",
            email: nil,
            active: false,
            plan: nil,
            planLabel: nil,
            headline: "Grok",
            error: "401 unauthorized",
            notes: nil,
            meters: []
        )

        let summary = summarize([account])
        XCTAssertEqual(summary.level, .bad)
        XCTAssertEqual(summary.text, "needs attention")
        XCTAssertEqual(summary.fraction, 1)
    }

    func testMeterThresholdsAndSummaryUseTightestQuota() {
        XCTAssertEqual(meterLevel(usedPct: 69), .good)
        XCTAssertEqual(meterLevel(usedPct: 71), .warn)
        XCTAssertEqual(meterLevel(usedPct: 91), .bad)

        let accounts = Account.samples
        let summary = summarize(accounts)
        XCTAssertEqual(summary.level, .bad)
        XCTAssertTrue(summary.text.contains("0% left"))
        XCTAssertEqual(Set(Account.largeSamples.map(\.id)).count, 20)
    }

    func testProcessRunnerCapturesOutputAndCannotDeadlockOnLargeStderr() throws {
        let data = try UsageModel.runProcessBlocking(
            url: URL(fileURLWithPath: "/bin/sh"),
            args: ["-c", "head -c 1048576 /dev/zero >&2; printf ok"],
            timeout: 5
        )
        XCTAssertEqual(String(decoding: data, as: UTF8.self), "ok")
    }

    func testProcessRunnerTerminatesAtDeadline() {
        let started = Date()
        XCTAssertThrowsError(try UsageModel.runProcessBlocking(
            url: URL(fileURLWithPath: "/bin/sh"),
            args: ["-c", "sleep 10"],
            timeout: 0.1
        )) { error in
            XCTAssertTrue(error.localizedDescription.contains("timed out"))
        }
        XCTAssertLessThan(Date().timeIntervalSince(started), 3)
    }

    func testProcessRunnerRespondsToTaskCancellation() async {
        let task = Task.detached {
            try UsageModel.runProcessBlocking(
                url: URL(fileURLWithPath: "/bin/sh"),
                args: ["-c", "sleep 10"],
                timeout: 20
            )
        }
        try? await Task.sleep(for: .milliseconds(100))
        let started = Date()
        task.cancel()
        do {
            _ = try await task.value
            XCTFail("cancelled process unexpectedly succeeded")
        } catch is CancellationError {
            XCTAssertLessThan(Date().timeIntervalSince(started), 3)
        } catch {
            XCTFail("expected CancellationError, got \(error)")
        }
    }

    func testProcessRunnerReturnsBoundedNonzeroDiagnostics() {
        XCTAssertThrowsError(try UsageModel.runProcessBlocking(
            url: URL(fileURLWithPath: "/bin/sh"),
            args: ["-c", "printf denied >&2; exit 7"],
            timeout: 5
        )) { error in
            XCTAssertTrue(error.localizedDescription.contains("code 7"))
            XCTAssertTrue(error.localizedDescription.contains("denied"))
        }
    }
}
