import AppKit
@testable import CmuxNextPages
import CmuxNextSettings
import Foundation
import Testing
import WebKit

/// The one prewarmed page host, in a real WKWebView with the real shell build (GUI host only:
/// cmux-lawrence-2). Page A is the shell's probe page acting as a page: it writes every store it
/// can, a global, a pending call and a subscription; page B must see none of it, after a reset in
/// the same host and on a new host after a release.
@MainActor
@Suite(.serialized, .timeLimit(.minutes(3))) struct PageHostPoolTests {
    /// Serves the probe's namespace: `slow` stays pending, `events` is a stream it can still emit on.
    final class ProbeProvider: PageProvider {
        var calls: [String] = []
        var cancelled = 0
        var emit: (@MainActor (JSONValue) -> Void)?
        var slow: CheckedContinuation<JSONValue, any Error>?
        var slowArrived: CheckedContinuation<Void, Never>?

        func call(_ op: String, params: JSONValue, context: PageCallContext) async throws -> JSONValue {
            calls.append(op)
            guard op == "cmux.shell.probe.slow" else { return ["ok": true] }
            slowArrived?.resume()
            slowArrived = nil
            return try await withCheckedThrowingContinuation { slow = $0 }
        }

        func subscribe(_ stream: String, filter: JSONValue, context: PageCallContext,
                       onEvent: @escaping @MainActor (JSONValue) -> Void) async throws -> PageSubscription {
            emit = onEvent
            return PageSubscription { [weak self] in self?.cancelled += 1 }
        }

        func waitForSlow() async {
            guard slow == nil else { return }
            await withCheckedContinuation { slowArrived = $0 }
        }

        func finish() {
            slow?.resume(returning: ["late": true])
            slow = nil
        }
    }

    static let pageA = """
    const ctx = globalThis.__cmuxShellProbe;
    localStorage.setItem('a', '1');
    sessionStorage.setItem('a', '1');
    globalThis.leakedByA = { token: 'a' };
    let idb = false;
    try {
      idb = await new Promise((resolve) => {
        const open = indexedDB.open('a-db', 1);
        open.onupgradeneeded = () => open.result.createObjectStore('s');
        open.onerror = () => resolve(false);
        open.onsuccess = () => {
          const db = open.result;
          const tx = db.transaction('s', 'readwrite');
          tx.objectStore('s').put('secret', 'k');
          tx.oncomplete = () => { db.close(); resolve(true); };
          tx.onerror = () => resolve(false);
        };
      });
    } catch { idb = false; }
    await ctx.client.subscribe('cmux.shell.probe.events', () => { document.documentElement.dataset.lateEvent = '1'; });
    return idb;
    """

    static let pendingCall = """
    try { await globalThis.__cmuxShellProbe.client.call('cmux.shell.probe.slow', {}); return 'resolved'; }
    catch (error) { return error.code || 'other'; }
    """

    static let pageB = """
    let databases = null;
    try { databases = indexedDB.databases ? (await indexedDB.databases()).map((d) => d.name) : null; } catch { databases = null; }
    return JSON.stringify({
      local: localStorage.length, session: sessionStorage.length, global: typeof globalThis.leakedByA,
      databases, late: document.documentElement.dataset.lateEvent ?? null,
      mounted: document.querySelectorAll('[data-shell-page]').length
    });
    """

    struct Seen: Decodable, Equatable {
        var local: Int
        var session: Int
        var global: String
        var databases: [String]?
        var late: String?
        var mounted: Int
    }

    static func js(_ host: PageWebView, _ script: String) async throws -> Any? {
        try await host.webKitView.callAsyncJavaScript(script, contentWorld: .page)
    }

    static func seen(_ host: PageWebView) async throws -> Seen {
        let text = try #require(try await js(host, pageB) as? String)
        return try JSONDecoder().decode(Seen.self, from: Data(text.utf8))
    }

    static func reply(_ send: (@escaping (Result<JSONValue, PageError>) -> Void) -> Void) async -> Result<JSONValue, PageError> {
        await withCheckedContinuation { continuation in send { continuation.resume(returning: $0) } }
    }

    /// A pool claim that returns once the shell has mounted the page.
    static func claim(_ pool: PageHostPool, _ descriptor: PageDescriptor, routes: [PageRoute] = [],
                      window: NSWindow) async throws -> PageWebView {
        var host: PageWebView?
        let result = await reply { done in
            host = pool.claim(descriptor, routes: routes, window: window, mounted: done)
            if host == nil { done(.failure(.closed)) }
        }
        #expect(result == .success(["page": .string(descriptor.id)]))
        return try #require(host)
    }

    static func loadedHost() async throws -> PageWebView {
        PageID.registerBundledRoot(PageShellFixture.webviewsApp, for: PageDescriptor.shell.id)
        let host = try #require(PageWebView(pooledHost: .shell))
        await host.waitUntilLoaded()
        await host.preloadShellPages()
        return host
    }

    /// Page A acts, then the page goes away (`leave`), then page B is mounted (by `next`, which
    /// returns its host): B reads nothing of A, A's stream is cancelled, A's pending call is closed.
    func checkNothingLeaks(provider: ProbeProvider, host: PageWebView, leave: () async -> Void,
                           next: () async throws -> PageWebView) async throws {
        let wroteIDB = try await Self.js(host, Self.pageA) as? Bool ?? false
        let outcome = Task { try await Self.js(host, Self.pendingCall) as? String }
        await provider.waitForSlow()
        #expect(host.router.subscriptionCount == 1)

        await leave()
        #expect(provider.cancelled == 1)
        #expect(host.router.subscriptionCount == 0)
        // A's stream and A's call answer late: nothing reaches any page.
        provider.emit?(["late": true])
        provider.finish()

        let b = try await next()
        let seen = try await Self.seen(b)
        #expect(seen.local == 0 && seen.session == 0)
        #expect(seen.global == "undefined")
        #expect(seen.late == nil)
        #expect(seen.mounted == 1)
        if wroteIDB { #expect(seen.databases == []) }
        // A's pending call: closed in the same host; gone with its document on a retired host.
        if b === host { #expect(try await outcome.value == "cmux.protocol.closed") } else { outcome.cancel() }
    }

    @Test func aResetLeavesTheNextPageNothingOfTheLast() async throws {
        let host = try await Self.loadedHost()
        defer { host.close() }
        let provider = ProbeProvider()
        let routes = [PageRoute(prefix: "cmux.shell.probe.", provider: provider)]
        host.retarget(descriptor: .shellProbe, routes: routes)
        #expect(await Self.reply { host.sendClaim(reply: $0) } == .success(["page": "cmux.shell.probe"]))
        try await checkNothingLeaks(provider: provider, host: host, leave: {
            _ = await Self.reply { host.resetShellPage(reply: $0) }
        }, next: {
            host.retarget(descriptor: .shellProbe, routes: [])
            _ = await Self.reply { host.sendClaim(reply: $0) }
            return host
        })
    }

    @Test func aNewHostAfterAReleaseCannotSeeTheOldHostsStorage() async throws {
        PageID.registerBundledRoot(PageShellFixture.webviewsApp, for: PageDescriptor.shell.id)
        let window = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 320, height: 240), styleMask: [.borderless],
                              backing: .buffered, defer: true)
        var policy = PageHostPool.Policy()
        policy.idleInput = .milliseconds(5)
        let pool = PageHostPool(policy: policy, activity: { 0 }, isTrackingMenu: { false })
        var ready: [CheckedContinuation<Void, Never>] = []
        pool.onSpareReady = { _ in ready.forEach { $0.resume() }; ready.removeAll() }
        func spare() async {
            if pool.isSpareReady { return }
            await withCheckedContinuation { ready.append($0) }
        }
        #expect(pool.claim(.shellProbe, routes: [], window: window) == nil)
        await spare()
        let provider = ProbeProvider()
        let a = try await Self.claim(pool, .shellProbe, routes: [PageRoute(prefix: "cmux.shell.probe.", provider: provider)],
                                     window: window)
        try await checkNothingLeaks(provider: provider, host: a, leave: {
            #expect(a.touched)
            pool.release(a)
            #expect(pool.spareHost !== a)
        }, next: {
            await spare()
            let b = try await Self.claim(pool, .shellProbe, window: window)
            #expect(b !== a)
            return b
        })
        #expect(pool.spans.contains { $0.name == "pool.makeSpare" })
        pool.dropSpare()
        pool.claimedHost.map(pool.release)
        window.close()
    }

    @Test func onlyAnUntouchedHostIsRecycled() async throws {
        PageID.registerBundledRoot(PageShellFixture.webviewsApp, for: PageDescriptor.shell.id)
        let window = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 320, height: 240), styleMask: [.borderless],
                              backing: .buffered, defer: true)
        var policy = PageHostPool.Policy()
        policy.idleInput = .milliseconds(5)
        let pool = PageHostPool(policy: policy, activity: { 0 }, isTrackingMenu: { false })
        var ready: [CheckedContinuation<Void, Never>] = []
        pool.onSpareReady = { _ in ready.forEach { $0.resume() }; ready.removeAll() }
        func spare() async {
            if pool.isSpareReady { return }
            await withCheckedContinuation { ready.append($0) }
        }
        pool.follow(window)
        pool.noteLikely()
        await spare()
        // Untouched: claimed and released with no op and no event; reset and parked again.
        let first = try await Self.claim(pool, .shellProbe, window: window)
        #expect(!first.touched)
        pool.release(first)
        #expect(pool.spareHost === first)
        #expect(pool.isSpareReady)
        #expect(first.descriptor.id == "cmux.shell")
        // One op: never recycled.
        let second = try await Self.claim(pool, .shellProbe, window: window)
        #expect(second === first)
        _ = try await Self.js(second, "try { await globalThis.__cmuxShellProbe.client.call('cmux.shell.probe.x', {}); } catch {} return 1")
        #expect(second.touched)
        pool.release(second)
        #expect(pool.spareHost !== second)
        pool.dropSpare()
        window.close()
    }

    @Test func theSpareWaitsForAQuietPeriod() async throws {
        PageID.registerBundledRoot(PageShellFixture.webviewsApp, for: PageDescriptor.shell.id)
        let window = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 320, height: 240), styleMask: [.borderless],
                              backing: .buffered, defer: true)
        var policy = PageHostPool.Policy()
        policy.idleInput = .milliseconds(5)
        var reads = 0
        // Busy (terminal output, frames, input) for the first deadlines, then quiet.
        let pool = PageHostPool(policy: policy, activity: { reads += 1; return UInt64(min(reads, 6)) }, isTrackingMenu: { false })
        var built: CheckedContinuation<Void, Never>?
        pool.onSpareReady = { _ in built?.resume(); built = nil }
        pool.follow(window)
        #expect(pool.spareHost == nil)
        pool.noteLikely()
        await withCheckedContinuation { built = $0 }
        #expect(reads >= 7)
        #expect(pool.isSpareReady)
        pool.dropSpare()
        window.close()
    }
}
