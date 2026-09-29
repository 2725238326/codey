import AppKit
import Foundation

// AppKit and screenshot callbacks need a running main loop while MCP reads stdin.
final class ComputerUseApplication: NSObject, NSApplicationDelegate {
    private let server = StdioMCPServer()

    func applicationDidFinishLaunching(_ notification: Notification) {
        Thread.detachNewThreadSelector(#selector(serve), toTarget: self, with: nil)
    }

    @objc private func serve() {
        do {
            try server.run()
        } catch {
            FileHandle.standardError.write(Data((String(describing: error) + String(UnicodeScalar(10))).utf8))
        }
        DispatchQueue.main.async { NSApp.terminate(nil) }
    }
}

let application = NSApplication.shared
application.setActivationPolicy(.accessory)
let delegate = ComputerUseApplication()
application.delegate = delegate
application.run()
