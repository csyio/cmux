import AppKit
import WebKit

/// The WKWebView of a page. It reports each user event WebKit receives (keys, clicks, scrolls,
/// gestures) before handling it, so the page host knows a pooled page was used (``PageWebView/touched``).
/// It handles no key itself: keys still go through the app's one key dispatcher to WebKit.
final class PageEngineView: WKWebView {
    var onUserEvent: (() -> Void)?

    override func keyDown(with event: NSEvent) {
        onUserEvent?()
        super.keyDown(with: event)
    }

    override func mouseDown(with event: NSEvent) {
        onUserEvent?()
        super.mouseDown(with: event)
    }

    override func rightMouseDown(with event: NSEvent) {
        onUserEvent?()
        super.rightMouseDown(with: event)
    }

    override func otherMouseDown(with event: NSEvent) {
        onUserEvent?()
        super.otherMouseDown(with: event)
    }

    override func scrollWheel(with event: NSEvent) {
        onUserEvent?()
        super.scrollWheel(with: event)
    }

    override func magnify(with event: NSEvent) {
        onUserEvent?()
        super.magnify(with: event)
    }
}
