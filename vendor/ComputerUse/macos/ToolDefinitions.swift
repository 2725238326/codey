import Foundation
import CoreFoundation

func validateToolArguments(name: String, arguments: [String: Any]) throws {
    guard let tool = ToolDefinitions.all.first(where: { $0.name == name }) else {
        throw ComputerUseError.unsupportedTool(name)
    }
    for key in tool.inputSchema["required"] as? [String] ?? [] {
        guard arguments[key] != nil else { throw ComputerUseError.missingArgument(key) }
    }
    let properties = tool.inputSchema["properties"] as? [String: [String: Any]] ?? [:]
    for (key, value) in arguments {
        guard let schema = properties[key], matchesToolValue(value, schema: schema) else {
            throw ComputerUseError.invalidArguments("Invalid argument: \(key)")
        }
    }
}

private func matchesToolValue(_ value: Any, schema: [String: Any]) -> Bool {
    if let variants = schema["anyOf"] as? [[String: Any]] {
        return variants.contains { matchesToolValue(value, schema: $0) }
    }
    if schema["type"] as? String == "string" {
        guard let value = value as? String else { return false }
        return (schema["enum"] as? [String]).map { $0.contains(value) } ?? true
    }
    guard let number = value as? NSNumber, CFGetTypeID(number) != CFBooleanGetTypeID() else { return false }
    let double = number.doubleValue
    guard double.isFinite, abs(double) < 9_007_199_254_740_992 else { return false }
    if schema["type"] as? String == "integer", double.rounded(.towardZero) != double { return false }
    if let minimum = schema["minimum"] as? NSNumber, double < minimum.doubleValue { return false }
    if let maximum = schema["maximum"] as? NSNumber, double > maximum.doubleValue { return false }
    return true
}

public struct ToolDefinition: @unchecked Sendable {
    public let name: String
    public let description: String
    public let annotations: [String: Any]
    public let inputSchema: [String: Any]

    public init(name: String, description: String, annotations: [String: Any], inputSchema: [String: Any]) {
        self.name = name
        self.description = description
        self.annotations = annotations
        self.inputSchema = inputSchema
    }

    public var asDictionary: [String: Any] {
        var dictionary: [String: Any] = [
            "name": name,
            "description": description,
            "inputSchema": inputSchema,
        ]

        if !annotations.isEmpty {
            dictionary["annotations"] = annotations
        }

        return dictionary
    }
}

public enum ToolDefinitions {
    public static let all: [ToolDefinition] = [
        ToolDefinition(
            name: "click",
            description: "Click an element by index or pixel coordinates from screenshot.",
            annotations: defaultAnnotations(),
            inputSchema: objectSchema(
                properties: [
                    "app": stringProperty(description: "App name or bundle identifier"),
                    "element_index": elementIndexProperty(description: "Element index to click"),
                    "x": numberProperty(description: "X coordinate in screenshot pixel coordinates"),
                    "y": numberProperty(description: "Y coordinate in screenshot pixel coordinates"),
                    "click_count": integerProperty(description: "Number of clicks. Defaults to 1"),
                    "mouse_button": stringProperty(
                        description: "Mouse button to click. Defaults to left.",
                        enumValues: ["left", "right", "middle"]
                    ),
                    "click_method": stringProperty(
                        description: "Click implementation: auto (default), accessibility, app_post, sky_click, or global. Accessibility requires element_index. app_post sends a public event directly to the target app. sky_click uses the macOS SkyLight background window path. Global may move the system pointer and requires OPEN_COMPUTER_USE_ALLOW_GLOBAL_POINTER_FALLBACKS=1.",
                        enumValues: ClickMethod.allCases.map(\.rawValue)
                    ),
                ],
                required: ["app"]
            )
        ),
        ToolDefinition(
            name: "drag",
            description: "Drag from one point to another using pixel coordinates. By default mouse events are posted directly to the target app and the system pointer does not move; that path cannot drive window-server drag sessions such as window moves, text selection, or Finder drag-and-drop. Those require OPEN_COMPUTER_USE_ALLOW_GLOBAL_POINTER_FALLBACKS=1 in the server process environment, which may move the real pointer. The result reports which path was used.",
            annotations: defaultAnnotations(),
            inputSchema: objectSchema(
                properties: [
                    "app": stringProperty(description: "App name or bundle identifier"),
                    "from_x": numberProperty(description: "Start X coordinate"),
                    "from_y": numberProperty(description: "Start Y coordinate"),
                    "to_x": numberProperty(description: "End X coordinate"),
                    "to_y": numberProperty(description: "End Y coordinate"),
                ],
                required: ["app", "from_x", "from_y", "to_x", "to_y"]
            )
        ),
        ToolDefinition(
            name: "get_app_state",
            description: "Get the app's key window screenshot and accessibility tree; may launch or activate the app when needed. Call this before acting and refresh after navigation.",
            annotations: ["readOnlyHint": false, "destructiveHint": false, "openWorldHint": false],
            inputSchema: objectSchema(
                properties: [
                    "app": stringProperty(description: "App name or bundle identifier"),
                    "text_limit": textLimitProperty(description: "Maximum text characters to return. Use \"max\" for full text. Defaults to 500."),
                    "max_tree_nodes": positiveIntegerProperty(description: "Maximum accessibility tree nodes to render. Defaults to 1200."),
                    "max_tree_depth": positiveIntegerProperty(description: "Maximum accessibility tree depth to render. Defaults to 64."),
                ],
                required: ["app"]
            )
        ),
        ToolDefinition(
            name: "list_apps",
            description: "List the apps on this computer. Returns the set of apps that are currently running, as well as any that have been used in the last 14 days, including details on usage frequency.",
            annotations: readOnlyAnnotations(),
            inputSchema: objectSchema(properties: [:], required: [])
        ),
        ToolDefinition(
            name: "perform_secondary_action",
            description: "Invoke a secondary accessibility action exposed by an element.",
            annotations: defaultAnnotations(),
            inputSchema: objectSchema(
                properties: [
                    "app": stringProperty(description: "App name or bundle identifier"),
                    "element_index": elementIndexProperty(description: "Element identifier"),
                    "action": stringProperty(description: "Secondary accessibility action name"),
                ],
                required: ["app", "element_index", "action"]
            )
        ),
        ToolDefinition(
            name: "press_key",
            description: "Press a key or key-combination on the keyboard, including modifier and navigation keys.\n  - This supports xdotool's `key` syntax.\n  - Examples: \"a\", \"Return\", \"Tab\", \"super+c\", \"Up\", \"KP_0\" (for the numpad 0 key).",
            annotations: defaultAnnotations(),
            inputSchema: objectSchema(
                properties: [
                    "app": stringProperty(description: "App name or bundle identifier"),
                    "key": stringProperty(description: "Key or key combination to press"),
                ],
                required: ["app", "key"]
            )
        ),
        ToolDefinition(
            name: "scroll",
            description: "Scroll an element in a direction by a number of pages.",
            annotations: defaultAnnotations(),
            inputSchema: objectSchema(
                properties: [
                    "app": stringProperty(description: "App name or bundle identifier"),
                    "direction": stringProperty(description: "Scroll direction: up, down, left, or right"),
                    "element_index": elementIndexProperty(description: "Element identifier"),
                    "pages": numberProperty(description: "Number of pages to scroll. Fractional values are supported. Defaults to 1"),
                ],
                required: ["app", "element_index", "direction"]
            )
        ),
        ToolDefinition(
            name: "set_value",
            description: "Set the value of a settable accessibility element.",
            annotations: defaultAnnotations(),
            inputSchema: objectSchema(
                properties: [
                    "app": stringProperty(description: "App name or bundle identifier"),
                    "element_index": elementIndexProperty(description: "Element identifier"),
                    "value": stringProperty(description: "Value to assign"),
                ],
                required: ["app", "element_index", "value"]
            )
        ),
        ToolDefinition(
            name: "type_text",
            description: "Type literal text using keyboard input.",
            annotations: defaultAnnotations(),
            inputSchema: objectSchema(
                properties: [
                    "app": stringProperty(description: "App name or bundle identifier"),
                    "text": stringProperty(description: "Literal text to type"),
                ],
                required: ["app", "text"]
            )
        ),
    ]
}

private func objectSchema(properties: [String: Any], required: [String]) -> [String: Any] {
    var schema: [String: Any] = [
        "type": "object",
        "properties": properties,
        "additionalProperties": false,
    ]

    if !required.isEmpty {
        schema["required"] = required
    }

    return schema
}

private func defaultAnnotations() -> [String: Any] {
    [
        "readOnlyHint": false,
        "destructiveHint": true,
        "openWorldHint": true,
    ]
}

private func readOnlyAnnotations() -> [String: Any] {
    [
        "destructiveHint": false,
        "idempotentHint": true,
        "openWorldHint": false,
        "readOnlyHint": true,
    ]
}

private func stringProperty(description: String, enumValues: [String]? = nil) -> [String: Any] {
    var property: [String: Any] = [
        "type": "string",
        "description": description,
    ]

    if let enumValues {
        property["enum"] = enumValues
    }

    return property
}

private func integerProperty(description: String) -> [String: Any] {
    [
        "type": "integer",
        "minimum": 1,
        "maximum": 3,
        "description": description,
    ]
}

private func elementIndexProperty(description: String) -> [String: Any] {
    ["anyOf": [["type": "string"], ["type": "integer", "minimum": 0]], "description": description]
}

private func positiveIntegerProperty(description: String) -> [String: Any] {
    [
        "type": "integer",
        "minimum": 1,
        "description": description,
    ]
}

private func textLimitProperty(description: String) -> [String: Any] {
    [
        "anyOf": [
            [
                "type": "integer",
                "minimum": 1,
            ],
            [
                "type": "string",
                "enum": [SnapshotTextLimit.maxKeyword],
            ],
        ],
        "description": description,
    ]
}

private func numberProperty(description: String) -> [String: Any] {
    [
        "type": "number",
        "description": description,
    ]
}
