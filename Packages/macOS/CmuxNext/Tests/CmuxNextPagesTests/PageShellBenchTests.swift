import AppKit
import CmuxNextPages
import CmuxNextSettings
import Foundation
import QuartzCore
import Testing
import WebKit

/// The claim bench (GUI host only, cmux-lawrence-2; opt in with CMUX_PAGE_SHELL_BENCH=1): the icon
/// picker claimed from a parked spare in the same window and across windows, timed from the claim
/// to the page's next animation frame after the mount, plus the footprint of one parked host.
/// Prints one `PAGE_SHELL_BENCH {json}` line and writes it to $NX_ARTIFACTS when set. Two small
/// non-activating panels; the test never activates the app.
@MainActor
@Suite(.serialized, .enabled(if: ProcessInfo.processInfo.environment["CMUX_PAGE_SHELL_BENCH"] == "1"))
struct PageShellBenchTests {
    static let rounds = 20

    func panel(x: CGFloat) -> NSPanel {
        let panel = NSPanel(contentRect: NSRect(x: x, y: 40, width: 352, height: 420),
                            styleMask: [.titled, .nonactivatingPanel], backing: .buffered, defer: false)
        panel.contentView = NSView(frame: NSRect(x: 0, y: 0, width: 352, height: 420))
        panel.orderFrontRegardless()
        return panel
    }

    static func stats(_ values: [Double]) -> [String: Double] {
        let sorted = values.sorted()
        func at(_ q: Double) -> Double { sorted[min(sorted.count - 1, Int(q * Double(sorted.count)))] }
        return ["n": Double(sorted.count), "p50": at(0.5), "p95": at(0.95), "max": sorted.last ?? 0]
    }

    /// `footprint` of `pid` in MB (the "phys_footprint" summary line), or nil.
    static func footprintMB(_ pid: pid_t) -> Double? {
        let process = Process()
        process.executableURL = URL(fileURLWithPath: "/usr/bin/footprint")
        process.arguments = ["\(pid)"]
        let pipe = Pipe()
        process.standardOutput = pipe
        process.standardError = Pipe()
        guard (try? process.run()) != nil else { return nil }
        let data = pipe.fileHandleForReading.readDataToEndOfFile()
        process.waitUntilExit()
        let text = String(decoding: data, as: UTF8.self)
        guard let line = text.split(separator: "\n").first(where: { $0.contains("Footprint:") }) else { return nil }
        let parts = line.split(separator: " ").map(String.init)
        guard let index = parts.firstIndex(of: "Footprint:"), index + 2 < parts.count, let value = Double(parts[index + 1]) else { return nil }
        switch parts[index + 2].uppercased() {
        case "KB": return value / 1024
        case "GB": return value * 1024
        default: return value
        }
    }

    @Test func claimToFirstFrame() async throws {
        NSApplication.shared.setActivationPolicy(.accessory)
        PageID.registerBundledRoot(PageShellFixture.webviewsApp, for: PageDescriptor.shell.id)
        let parked = panel(x: 40)
        let other = panel(x: 420)
        defer { parked.close(); other.close() }
        var policy = PageHostPool.Policy()
        policy.idleInput = .milliseconds(5)
        let pool = PageHostPool(policy: policy, activity: { 0 }, isTrackingMenu: { false })
        var ready: [CheckedContinuation<Void, Never>] = []
        pool.onSpareReady = { _ in ready.forEach { $0.resume() }; ready.removeAll() }
        func spare() async {
            if pool.isSpareReady { return }
            await withCheckedContinuation { ready.append($0) }
        }
        let testProcessBefore = Self.footprintMB(getpid())
        pool.follow(parked)
        pool.noteLikely()
        await spare()
        let spareHost = try #require(pool.spareHost)
        let webPID = (spareHost.webKitView.value(forKey: "_webProcessIdentifier") as? NSNumber)?.int32Value
        let webContentMB = webPID.flatMap { Self.footprintMB($0) }
        let testProcessAfter = Self.footprintMB(getpid())

        var results: [String: Any] = [:]
        for (label, window) in [("sameWindow", parked), ("crossWindow", other)] {
            var claimMs: [Double] = []
            var firstFrameMs: [Double] = []
            for round in 0..<Self.rounds {
                await spare()
                let session: JSONValue = ["id": .string("bench-\(round)"), "tab": "emoji"]
                let start = CACurrentMediaTime()
                let host = try #require(pool.claim(.iconPicker, routes: [], context: session, window: window))
                if let content = window.contentView {
                    host.frame = content.bounds
                    content.addSubview(host)
                }
                claimMs.append((CACurrentMediaTime() - start) * 1000)
                let mounted = try await host.webKitView.callAsyncJavaScript(
                    "await new Promise((r) => requestAnimationFrame(() => r())); return document.querySelectorAll('.icon-cell').length",
                    contentWorld: .page) as? Int ?? 0
                firstFrameMs.append((CACurrentMediaTime() - start) * 1000)
                #expect(mounted > 0, "the picker rendered no cells")
                pool.release(host)
            }
            results[label] = ["claimMs": Self.stats(claimMs), "claimToFirstFrameMs": Self.stats(firstFrameMs)]
        }
        results["makeSpareMs"] = Self.stats(pool.spans.filter { $0.name == "pool.makeSpare" }.map(\.milliseconds))
        results["parkMs"] = Self.stats(pool.spans.filter { $0.name == "pool.makeSpare.park" }.map(\.milliseconds))
        results["webContentFootprintMB"] = webContentMB ?? -1
        results["testProcessFootprintMB"] = ["before": testProcessBefore ?? -1, "afterOneHost": testProcessAfter ?? -1]
        let json = try JSONSerialization.data(withJSONObject: results, options: [.sortedKeys])
        let line = "PAGE_SHELL_BENCH " + String(decoding: json, as: UTF8.self)
        print(line)
        if let dir = ProcessInfo.processInfo.environment["NX_ARTIFACTS"] {
            try? Data(line.utf8).write(to: URL(fileURLWithPath: dir).appending(path: "page-shell-bench.json"))
        }
        pool.dropSpare()
    }
}
