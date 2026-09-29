import Foundation

func normalizedElementIndexArgument(_ value: Any?) -> String? {
    if let string = value as? String {
        return string.isEmpty ? nil : string
    }

    if let integer = value as? Int {
        return String(integer)
    }

    if let number = value as? NSNumber {
        if CFGetTypeID(number as CFTypeRef) == CFBooleanGetTypeID() {
            return nil
        }

        return normalizedElementIndexNumber(number.doubleValue)
    }

    if let double = value as? Double {
        return normalizedElementIndexNumber(double)
    }

    return nil
}

private func normalizedElementIndexNumber(_ value: Double) -> String? {
    guard value.isFinite, value.rounded(.towardZero) == value else {
        return nil
    }

    guard value >= Double(Int.min), value < Double(Int.max) else {
        return nil
    }

    return String(Int(value))
}

public final class ComputerUseToolDispatcher {
    private let service: ComputerUseService

    public init(service: ComputerUseService = ComputerUseService()) {
        self.service = service
    }

    public func callTool(name: String, arguments: [String: Any]) throws -> ToolCallResult {
        try validateToolArguments(name: name, arguments: arguments)
        switch name {
        case "list_apps":
            return service.listApps()
        case "get_app_state":
            return try service.getAppState(
                app: requireString("app", in: arguments),
                textLimit: try optionalTextLimit("text_limit", in: arguments) ?? .defaults,
                treeLimits: AccessibilityTreeLimits.defaults.replacing(
                    maxNodeCount: try optionalPositiveInt("max_tree_nodes", in: arguments),
                    maxDepth: try optionalPositiveInt("max_tree_depth", in: arguments)
                )
            )
        case "click":
            return try service.click(
                app: requireString("app", in: arguments),
                elementIndex: optionalElementIndex(in: arguments),
                x: optionalDouble("x", in: arguments),
                y: optionalDouble("y", in: arguments),
                clickCount: Int(optionalDouble("click_count", in: arguments) ?? 1),
                mouseButton: optionalString("mouse_button", in: arguments) ?? "left",
                clickMethod: try parseClickMethod(optionalString("click_method", in: arguments))
            )
        case "perform_secondary_action":
            return try service.performSecondaryAction(
                app: requireString("app", in: arguments),
                elementIndex: requireElementIndex(in: arguments),
                action: requireString("action", in: arguments)
            )
        case "scroll":
            return try service.scroll(
                app: requireString("app", in: arguments),
                direction: requireString("direction", in: arguments),
                elementIndex: requireElementIndex(in: arguments),
                pages: optionalDouble("pages", in: arguments) ?? 1
            )
        case "drag":
            return try service.drag(
                app: requireString("app", in: arguments),
                fromX: requireDouble("from_x", in: arguments),
                fromY: requireDouble("from_y", in: arguments),
                toX: requireDouble("to_x", in: arguments),
                toY: requireDouble("to_y", in: arguments)
            )
        case "type_text":
            return try service.typeText(
                app: requireString("app", in: arguments),
                text: requireString("text", in: arguments)
            )
        case "press_key":
            return try service.pressKey(
                app: requireString("app", in: arguments),
                key: requireString("key", in: arguments)
            )
        case "set_value":
            return try service.setValue(
                app: requireString("app", in: arguments),
                elementIndex: requireElementIndex(in: arguments),
                value: requireString("value", in: arguments)
            )
        default:
            throw ComputerUseError.unsupportedTool(name)
        }
    }

    private func requireString(_ key: String, in arguments: [String: Any]) throws -> String {
        guard let value = arguments[key] as? String, !value.isEmpty || key == "value" || key == "text" else {
            throw ComputerUseError.missingArgument(key)
        }

        return value
    }

    private func optionalString(_ key: String, in arguments: [String: Any]) -> String? {
        arguments[key] as? String
    }

    private func optionalTextLimit(_ key: String, in arguments: [String: Any]) throws -> SnapshotTextLimit? {
        guard let value = arguments[key] else {
            return nil
        }

        if let string = value as? String {
            guard string.lowercased() == SnapshotTextLimit.maxKeyword else {
                throw ComputerUseError.invalidArguments("\(key) must be a positive integer or max")
            }
            return .max
        }

        let maxCount = try positiveInt(from: value, key: key, expectedDescription: "a positive integer or max")
        return SnapshotTextLimit(maxCount: maxCount)
    }

    private func requireElementIndex(in arguments: [String: Any]) throws -> String {
        guard let value = optionalElementIndex(in: arguments) else {
            throw ComputerUseError.missingArgument("element_index")
        }

        return value
    }

    private func optionalElementIndex(in arguments: [String: Any]) -> String? {
        normalizedElementIndexArgument(arguments["element_index"])
    }

    private func requireDouble(_ key: String, in arguments: [String: Any]) throws -> Double {
        guard let value = optionalDouble(key, in: arguments) else {
            throw ComputerUseError.missingArgument(key)
        }

        return value
    }

    private func optionalDouble(_ key: String, in arguments: [String: Any]) -> Double? {
        if let double = arguments[key] as? Double {
            return double
        }

        if let integer = arguments[key] as? Int {
            return Double(integer)
        }

        if let number = arguments[key] as? NSNumber {
            return number.doubleValue
        }

        return nil
    }

    private func optionalPositiveInt(_ key: String, in arguments: [String: Any]) throws -> Int? {
        guard let value = arguments[key] else {
            return nil
        }

        return try positiveInt(from: value, key: key, expectedDescription: "a positive integer")
    }

    private func positiveInt(from value: Any, key: String, expectedDescription: String) throws -> Int {
        if let integer = value as? Int {
            return try validatePositiveInt(integer, key: key, expectedDescription: expectedDescription)
        }

        if let double = value as? Double {
            return try validatePositiveWholeNumber(double, key: key, expectedDescription: expectedDescription)
        }

        if let number = value as? NSNumber {
            if CFGetTypeID(number as CFTypeRef) == CFBooleanGetTypeID() {
                throw ComputerUseError.invalidArguments("\(key) must be \(expectedDescription)")
            }
            return try validatePositiveWholeNumber(number.doubleValue, key: key, expectedDescription: expectedDescription)
        }

        throw ComputerUseError.invalidArguments("\(key) must be \(expectedDescription)")
    }

    private func validatePositiveWholeNumber(_ value: Double, key: String, expectedDescription: String) throws -> Int {
        guard value.isFinite, value.rounded(.towardZero) == value else {
            throw ComputerUseError.invalidArguments("\(key) must be \(expectedDescription)")
        }

        guard value >= Double(Int.min), value < Double(Int.max) else {
            throw ComputerUseError.invalidArguments("\(key) is outside the supported integer range")
        }

        return try validatePositiveInt(Int(value), key: key, expectedDescription: expectedDescription)
    }

    private func validatePositiveInt(_ value: Int, key: String, expectedDescription: String) throws -> Int {
        guard value > 0 else {
            throw ComputerUseError.invalidArguments("\(key) must be \(expectedDescription)")
        }
        return value
    }
}
