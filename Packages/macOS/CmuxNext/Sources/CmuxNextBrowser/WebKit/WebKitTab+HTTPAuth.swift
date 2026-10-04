import AppKit
import CmuxNextDesign
import Foundation
import WebKit

/// The WebKit tab's HTTP sign-in (`BrowserHTTPAuth`).
extension WebKitTab {
    func askHTTPCredentials(_ challenge: URLAuthenticationChallenge,
                            completionHandler: @escaping @MainActor (URLSession.AuthChallengeDisposition, URLCredential?) -> Void) {
        let space = challenge.protectionSpace
        let port = space.port == 0 || space.port == 80 || space.port == 443 ? "" : ":\(space.port)"
        let spec = BrowserHTTPAuth.spec(host: space.host + port, realm: space.realm,
                                       isSecure: space.receivesCredentialSecurely, failedBefore: challenge.previousFailureCount > 0,
                                       user: challenge.proposedCredential?.user)
        guard contentView.window != nil else { return completionHandler(.performDefaultHandling, nil) }
        CmuxDialogCenter.shared.present(spec, in: .tab(contentView)) { answer in
            if let credential = BrowserHTTPAuth.credential(for: answer) {
                completionHandler(.useCredential, credential)
            } else {
                completionHandler(.performDefaultHandling, nil)
            }
        }
    }
}
