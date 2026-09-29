import Foundation
import CoreFoundation

let computerUseServerInstructions = """
Computer Use tools let you interact with macOS apps by performing UI actions.

Some apps might have a separate dedicated plugin or skill. You may want to use that plugin or skill instead of Computer Use when it seems like a good fit for the task. While the separate plugin or skill may not expose every feature in the app, if the plugin can perform the task with its available features, prefer it. If the needed capability is not exposed there, use Computer Use may be appropriate for the missing interaction.

Call `get_app_state` before interacting with an app and whenever its UI changes. Element indices belong to the most recent snapshot and must not be reused after navigation.

The available tools are list_apps, get_app_state, click, perform_secondary_action, scroll, drag, type_text, press_key, and set_value.

Tools prefer background actions. Reading state may launch or activate an app when needed. Global pointer fallbacks are disabled unless explicitly enabled in the server environment. Avoid disrupting the user's session or overwriting their clipboard without authorization.

After each action, use the action result or fetch the latest state to verify the UI changed as expected.
Prefer element-targeted interactions over coordinate clicks when an index for the targeted element is available. Note that element indices are the sequential integers from the app state's accessibility tree.
Avoid falling back to AppleScript during a computer use session. Prefer Computer Use tools as much as possible to complete tasks.
Only perform destructive or externally visible actions such as sending, deleting, or purchasing when the user has authorized them. System accessibility and screen-recording permissions are required; never bypass these permissions.
"""

public final class StdioMCPServer {
    private let dispatcher: ComputerUseToolDispatcher

    public init(service: ComputerUseService = ComputerUseService()) {
        self.dispatcher = ComputerUseToolDispatcher(service: service)
    }

    public func run() throws {
        var pending = Data()
        var oversized = false
        func flush() throws {
            defer { pending.removeAll(keepingCapacity: true); oversized = false }
            let response: String?
            if oversized {
                response = try encodeJSONRPCError(id: nil, code: -32700, message: "Request exceeds 1 MiB")
            } else if let line = String(data: pending, encoding: .utf8) {
                response = line.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty ? nil : autoreleasepool { handle(line: line) }
            } else {
                response = try encodeJSONRPCError(id: nil, code: -32700, message: "Invalid UTF-8 JSON")
            }
            if let response {
                try FileHandle.standardOutput.write(contentsOf: Data((response + "\n").utf8))
            }
        }
        while let chunk = try FileHandle.standardInput.read(upToCount: 8192), !chunk.isEmpty {
            for byte in chunk {
                if byte == 10 {
                    try flush()
                } else if !oversized {
                    if pending.count == 1_048_576 { oversized = true } else { pending.append(byte) }
                }
            }
        }
        if oversized || !pending.isEmpty { try flush() }
    }

    public func handle(line: String) -> String? {
        let object: Any
        do { object = try JSONSerialization.jsonObject(with: Data(line.utf8), options: [.fragmentsAllowed]) }
        catch { return try? encodeJSONRPCError(id: nil, code: -32700, message: "Invalid JSON") }
        guard let payload = object as? [String: Any],
              payload["jsonrpc"] as? String == "2.0",
              let method = payload["method"] as? String, !method.isEmpty else {
            return try? encodeJSONRPCError(id: nil, code: -32600, message: "Invalid JSON-RPC request")
        }
        // Notifications never trigger actions and never receive responses.
        guard let id = payload["id"] else { return nil }
        let number = id as? NSNumber
        guard id is String || (number != nil && CFGetTypeID(number!) != CFBooleanGetTypeID()) else {
            return try? encodeJSONRPCError(id: nil, code: -32600, message: "Invalid request ID")
        }
        if let params = payload["params"], !(params is [String: Any]) {
            return try? encodeJSONRPCError(id: id, code: -32602, message: "params must be an object")
        }
        let params = payload["params"] as? [String: Any] ?? [:]
        do {
            switch method {
            case "initialize":
                return try encodeJSONRPCResult(
                    id: id,
                    result: [
                        "protocolVersion": "2025-03-26",
                        "serverInfo": [
                            "name": "codey-computer-use",
                            "version": "1.0.0",
                        ],
                        "capabilities": [
                            "tools": [
                                "listChanged": false,
                            ],
                        ],
                        "instructions": computerUseServerInstructions,
                    ]
                )
            case "ping":
                return try encodeJSONRPCResult(id: id, result: [:])
            case "tools/list":
                return try encodeJSONRPCResult(
                    id: id,
                    result: [
                        "tools": ToolDefinitions.all.map(\.asDictionary),
                    ]
                )
            case "tools/call":
                guard let name = params["name"] as? String, ToolDefinitions.all.contains(where: { $0.name == name }),
                      params["arguments"] == nil || params["arguments"] is [String: Any] else {
                    return try encodeJSONRPCError(id: id, code: -32602, message: "Unknown tool or invalid arguments")
                }
                let arguments = params["arguments"] as? [String: Any] ?? [:]
                let result = try dispatcher.callTool(name: name, arguments: arguments)
                return try encodeJSONRPCResult(
                    id: id,
                    result: result.asDictionary
                )
            default:
                return try encodeJSONRPCError(id: id, code: -32601, message: "Method not found")
            }
        } catch {
            let message = (error as? LocalizedError)?.errorDescription ?? String(describing: error)
            let result = ToolCallResult.text(
                message,
                isError: (error as? ComputerUseError)?.toolResultIsError ?? true
            )
            return try? encodeJSONRPCResult(id: id, result: result.asDictionary)
        }
    }

    private func encodeJSONRPCResult(id: Any?, result: [String: Any]) throws -> String {
        try encode([
            "jsonrpc": "2.0",
            "id": id ?? NSNull(),
            "result": result,
        ])
    }

    private func encodeJSONRPCError(id: Any?, code: Int, message: String) throws -> String {
        try encode([
            "jsonrpc": "2.0",
            "id": id ?? NSNull(),
            "error": [
                "code": code,
                "message": message,
            ],
        ])
    }

    private func encode(_ object: [String: Any]) throws -> String {
        let data = try JSONSerialization.data(withJSONObject: object, options: [.withoutEscapingSlashes])
        guard let text = String(data: data, encoding: .utf8) else {
            throw ComputerUseError.message("Failed to encode JSON-RPC response.")
        }

        return text
    }
}
