package main

import (
	"bufio"
	"bytes"
	"context"
	_ "embed"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"math"
	"os"
	"os/exec"
	"path/filepath"
	"runtime"
	"strconv"
	"strings"
	"time"
)

const version = "1.0.0"

var clickMethodValues = []string{"auto", "accessibility", "app_post", "sky_click", "global"}

//go:embed runtime.ps1
var windowsRuntimeScript string

const serverInstructions = "Computer Use tools let you interact with Windows apps by performing UI actions.\n\nBegin by calling `get_app_state` every turn you want to use Computer Use to get the latest state before acting. The available tools are list_apps, get_app_state, click, perform_secondary_action, scroll, drag, type_text, press_key, and set_value.\n\nPrefer element-targeted interactions over coordinate clicks when an index for the targeted element is available. Windows actions use UI Automation patterns first and fall back to window messages when an app does not expose the needed pattern. The Windows runtime does not auto-launch apps, perform SetFocus, or use UIA text fallback by default, so background-capable actions do not intentionally steal the user's foreground focus."

type toolDefinition struct {
	Name        string         `json:"name"`
	Description string         `json:"description"`
	Annotations map[string]any `json:"annotations,omitempty"`
	InputSchema map[string]any `json:"inputSchema"`
}

type contentItem struct {
	Type     string `json:"type"`
	Text     string `json:"text,omitempty"`
	Data     string `json:"data,omitempty"`
	MimeType string `json:"mimeType,omitempty"`
}

type toolCallResult struct {
	Content []contentItem `json:"content"`
	IsError bool          `json:"isError"`
}

func textResult(text string, isError bool) toolCallResult {
	return toolCallResult{Content: []contentItem{{Type: "text", Text: text}}, IsError: isError}
}

type appDescriptor struct {
	Name             string `json:"name"`
	BundleIdentifier string `json:"bundleIdentifier,omitempty"`
	PID              int    `json:"pid"`
}

type frame struct {
	X      float64 `json:"x"`
	Y      float64 `json:"y"`
	Width  float64 `json:"width"`
	Height float64 `json:"height"`
}

func (f frame) renderedLocalFrame() string {
	return fmt.Sprintf("{{x: %.0f, y: %.0f, width: %.0f, height: %.0f}}", f.X, f.Y, f.Width, f.Height)
}

type elementRecord struct {
	Index                int      `json:"index"`
	RuntimeID            []int    `json:"runtimeId,omitempty"`
	AutomationID         string   `json:"automationId,omitempty"`
	Name                 string   `json:"name,omitempty"`
	ControlType          string   `json:"controlType,omitempty"`
	LocalizedControlType string   `json:"localizedControlType,omitempty"`
	ClassName            string   `json:"className,omitempty"`
	Value                string   `json:"value,omitempty"`
	NativeWindowHandle   int64    `json:"nativeWindowHandle,omitempty"`
	Frame                *frame   `json:"frame,omitempty"`
	Actions              []string `json:"actions,omitempty"`
}

type appSnapshot struct {
	App                 appDescriptor   `json:"app"`
	WindowTitle         string          `json:"windowTitle,omitempty"`
	WindowBounds        *frame          `json:"windowBounds,omitempty"`
	ScreenshotPNGBase64 string          `json:"screenshotPngBase64,omitempty"`
	TreeLines           []string        `json:"treeLines,omitempty"`
	FocusedSummary      string          `json:"focusedSummary,omitempty"`
	SelectedText        string          `json:"selectedText,omitempty"`
	Elements            []elementRecord `json:"elements,omitempty"`
}

func (s *appSnapshot) renderedText() string {
	if s == nil {
		return ""
	}
	appRef := s.App.BundleIdentifier
	if appRef == "" {
		appRef = s.App.Name
	}
	title := s.WindowTitle
	if strings.TrimSpace(title) == "" {
		title = s.App.Name
	}

	lines := []string{
		fmt.Sprintf("App=%s (pid %d)", appRef, s.App.PID),
		fmt.Sprintf("Window: %q, App: %s.", title, s.App.Name),
	}
	lines = append(lines, s.TreeLines...)
	if strings.TrimSpace(s.SelectedText) != "" {
		lines = append(lines, "", fmt.Sprintf("Selected text: [%s]", s.SelectedText))
	} else if strings.TrimSpace(s.FocusedSummary) != "" {
		lines = append(lines, "", fmt.Sprintf("The focused UI element is %s.", s.FocusedSummary))
	}
	return strings.Join(lines, "\n")
}

func (s *appSnapshot) result() toolCallResult {
	result := toolCallResult{
		Content: []contentItem{{Type: "text", Text: s.renderedText()}},
	}
	if s != nil && s.ScreenshotPNGBase64 != "" {
		result.Content = append(result.Content, contentItem{
			Type:     "image",
			Data:     s.ScreenshotPNGBase64,
			MimeType: "image/png",
		})
	}
	return result
}

type psRequest struct {
	Tool         string         `json:"tool"`
	App          string         `json:"app,omitempty"`
	Element      *elementRecord `json:"element,omitempty"`
	X            *float64       `json:"x,omitempty"`
	Y            *float64       `json:"y,omitempty"`
	FromX        *float64       `json:"from_x,omitempty"`
	FromY        *float64       `json:"from_y,omitempty"`
	ToX          *float64       `json:"to_x,omitempty"`
	ToY          *float64       `json:"to_y,omitempty"`
	ClickCount   int            `json:"click_count,omitempty"`
	MouseButton  string         `json:"mouse_button,omitempty"`
	ClickMethod  string         `json:"click_method,omitempty"`
	Action       string         `json:"action,omitempty"`
	Direction    string         `json:"direction,omitempty"`
	Pages        float64        `json:"pages,omitempty"`
	Text         string         `json:"text,omitempty"`
	Key          string         `json:"key,omitempty"`
	Value        string         `json:"value,omitempty"`
	WindowBounds *frame         `json:"windowBounds,omitempty"`
	TextLimit    any            `json:"text_limit,omitempty"`
	MaxTreeNodes int            `json:"max_tree_nodes,omitempty"`
	MaxTreeDepth int            `json:"max_tree_depth,omitempty"`
}

type textLimit struct {
	max   bool
	count int
}

func (limit textLimit) runtimeValue() any {
	if limit.max {
		return "max"
	}
	return limit.count
}

type psResponse struct {
	OK       bool         `json:"ok"`
	Text     string       `json:"text,omitempty"`
	Error    string       `json:"error,omitempty"`
	Snapshot *appSnapshot `json:"snapshot,omitempty"`
}

type service struct {
	snapshots map[string]*appSnapshot
}

func newService() *service {
	return &service{snapshots: map[string]*appSnapshot{}}
}

func (s *service) callTool(tool toolDefinition, args map[string]any) toolCallResult {
	if err := tool.validateArguments(args); err != nil {
		return textResult(err.Error(), true)
	}
	switch tool.Name {
	case "list_apps":
		return s.listApps()
	case "get_app_state":
		maxTreeNodes, err := optionalPositiveInt(args, "max_tree_nodes")
		if err != nil {
			return textResult(err.Error(), true)
		}
		maxTreeDepth, err := optionalPositiveInt(args, "max_tree_depth")
		if err != nil {
			return textResult(err.Error(), true)
		}
		textLimit, err := optionalTextLimit(args, "text_limit")
		if err != nil {
			return textResult(err.Error(), true)
		}
		return s.getAppState(requiredString(args, "app"), textLimit, maxTreeNodes, maxTreeDepth)
	case "click":
		clickMethod, err := parseClickMethod(optionalString(args, "click_method"))
		if err != nil {
			return textResult(err.Error(), true)
		}
		return s.click(
			requiredString(args, "app"),
			optionalElementIndex(args),
			optionalFloat(args, "x"),
			optionalFloat(args, "y"),
			intValue(optionalFloat(args, "click_count"), 1),
			defaultString(optionalString(args, "mouse_button"), "left"),
			clickMethod,
		)
	case "perform_secondary_action":
		return s.performSecondaryAction(
			requiredString(args, "app"),
			requiredElementIndex(args),
			requiredString(args, "action"),
		)
	case "scroll":
		return s.scroll(
			requiredString(args, "app"),
			requiredString(args, "direction"),
			requiredElementIndex(args),
			floatValue(optionalFloat(args, "pages"), 1),
		)
	case "drag":
		return s.drag(
			requiredString(args, "app"),
			optionalFloat(args, "from_x"),
			optionalFloat(args, "from_y"),
			optionalFloat(args, "to_x"),
			optionalFloat(args, "to_y"),
		)
	case "type_text":
		return s.typeText(requiredString(args, "app"), requiredString(args, "text"))
	case "press_key":
		return s.pressKey(requiredString(args, "app"), requiredString(args, "key"))
	case "set_value":
		return s.setValue(requiredString(args, "app"), requiredElementIndex(args), requiredString(args, "value"))
	default:
		return textResult(fmt.Sprintf("unsupportedTool(%q)", tool.Name), true)
	}
}

func (s *service) listApps() toolCallResult {
	response, err := runPowerShell(psRequest{Tool: "list_apps"})
	if err != nil {
		return textResult(err.Error(), true)
	}
	if !response.OK {
		return textResult(response.Error, true)
	}
	if strings.TrimSpace(response.Text) == "" {
		response.Text = "No running top-level apps are visible to this Windows runtime."
	}
	return textResult(response.Text, false)
}

func (s *service) getAppState(app string, textLimit *textLimit, maxTreeNodes, maxTreeDepth *int) toolCallResult {
	if app == "" {
		return textResult("Missing required argument: app", true)
	}
	request := psRequest{Tool: "get_app_state", App: app}
	if textLimit != nil {
		request.TextLimit = textLimit.runtimeValue()
	}
	if maxTreeNodes != nil {
		request.MaxTreeNodes = *maxTreeNodes
	}
	if maxTreeDepth != nil {
		request.MaxTreeDepth = *maxTreeDepth
	}
	return s.snapshotResult(request)
}

func (s *service) click(app, elementIndex string, x, y *float64, clickCount int, mouseButton, clickMethod string) toolCallResult {
	if app == "" {
		return textResult("Missing required argument: app", true)
	}
	if elementIndex == "" && (x == nil || y == nil) {
		return textResult("click requires either element_index or x/y", true)
	}
	if clickMethod == "accessibility" && elementIndex == "" {
		return textResult("click_method 'accessibility' requires element_index", true)
	}
	if clickMethod == "global" {
		return textResult("click_method 'global' is not supported on Windows", true)
	}
	if clickMethod == "sky_click" {
		return textResult("click_method 'sky_click' is not supported on Windows", true)
	}
	snapshot := s.currentSnapshot(app)
	if snapshot == nil {
		return textResult("No app state is available for "+app+". Run get_app_state before action tools.", true)
	}
	request := psRequest{
		Tool:         "click",
		App:          app,
		X:            x,
		Y:            y,
		ClickCount:   clickCount,
		MouseButton:  mouseButton,
		ClickMethod:  clickMethod,
		WindowBounds: snapshot.WindowBounds,
	}
	if elementIndex != "" {
		record, err := lookupElement(snapshot, elementIndex)
		if err != nil {
			return textResult(err.Error(), true)
		}
		request.Element = record
	}
	return s.snapshotResult(request)
}

func (s *service) performSecondaryAction(app, elementIndex, action string) toolCallResult {
	if app == "" {
		return textResult("Missing required argument: app", true)
	}
	if elementIndex == "" {
		return textResult("Missing required argument: element_index", true)
	}
	if action == "" {
		return textResult("Missing required argument: action", true)
	}
	snapshot := s.currentSnapshot(app)
	if snapshot == nil {
		return textResult("No app state is available for "+app+". Run get_app_state before action tools.", true)
	}
	record, err := lookupElement(snapshot, elementIndex)
	if err != nil {
		return textResult(err.Error(), true)
	}
	return s.snapshotResult(psRequest{Tool: "perform_secondary_action", App: app, Element: record, Action: action})
}

func (s *service) scroll(app, direction, elementIndex string, pages float64) toolCallResult {
	if app == "" {
		return textResult("Missing required argument: app", true)
	}
	if elementIndex == "" {
		return textResult("Missing required argument: element_index", true)
	}
	normalized := strings.ToLower(direction)
	if normalized != "up" && normalized != "down" && normalized != "left" && normalized != "right" {
		return textResult("Invalid scroll direction: "+direction, true)
	}
	if pages <= 0 {
		return textResult("pages must be > 0", true)
	}
	snapshot := s.currentSnapshot(app)
	if snapshot == nil {
		return textResult("No app state is available for "+app+". Run get_app_state before action tools.", true)
	}
	record, err := lookupElement(snapshot, elementIndex)
	if err != nil {
		return textResult(err.Error(), true)
	}
	return s.snapshotResult(psRequest{Tool: "scroll", App: app, Element: record, Direction: normalized, Pages: pages})
}

func (s *service) drag(app string, fromX, fromY, toX, toY *float64) toolCallResult {
	if app == "" {
		return textResult("Missing required argument: app", true)
	}
	if fromX == nil {
		return textResult("Missing required argument: from_x", true)
	}
	if fromY == nil {
		return textResult("Missing required argument: from_y", true)
	}
	if toX == nil {
		return textResult("Missing required argument: to_x", true)
	}
	if toY == nil {
		return textResult("Missing required argument: to_y", true)
	}
	snapshot := s.currentSnapshot(app)
	if snapshot == nil {
		return textResult("No app state is available for "+app+". Run get_app_state before action tools.", true)
	}
	return s.snapshotResult(psRequest{Tool: "drag", App: app, FromX: fromX, FromY: fromY, ToX: toX, ToY: toY, WindowBounds: snapshot.WindowBounds})
}

func (s *service) typeText(app, text string) toolCallResult {
	if app == "" {
		return textResult("Missing required argument: app", true)
	}
	if text == "" {
		return textResult("Missing required argument: text", true)
	}
	if s.currentSnapshot(app) == nil {
		return textResult("No app state is available for "+app+". Run get_app_state before action tools.", true)
	}
	return s.snapshotResult(psRequest{Tool: "type_text", App: app, Text: text})
}

func (s *service) pressKey(app, key string) toolCallResult {
	if app == "" {
		return textResult("Missing required argument: app", true)
	}
	if key == "" {
		return textResult("Missing required argument: key", true)
	}
	if s.currentSnapshot(app) == nil {
		return textResult("No app state is available for "+app+". Run get_app_state before action tools.", true)
	}
	return s.snapshotResult(psRequest{Tool: "press_key", App: app, Key: key})
}

func (s *service) setValue(app, elementIndex, value string) toolCallResult {
	if app == "" {
		return textResult("Missing required argument: app", true)
	}
	if elementIndex == "" {
		return textResult("Missing required argument: element_index", true)
	}
	snapshot := s.currentSnapshot(app)
	if snapshot == nil {
		return textResult("No app state is available for "+app+". Run get_app_state before action tools.", true)
	}
	record, err := lookupElement(snapshot, elementIndex)
	if err != nil {
		return textResult(err.Error(), true)
	}
	return s.snapshotResult(psRequest{Tool: "set_value", App: app, Element: record, Value: value})
}

func (s *service) currentSnapshot(app string) *appSnapshot {
	return s.snapshots[strings.ToLower(app)]
}

func (s *service) snapshotResult(request psRequest) toolCallResult {
	response, err := runPowerShell(request)
	if err != nil {
		return textResult(err.Error(), true)
	}
	if !response.OK {
		return textResult(response.Error, true)
	}
	if response.Snapshot == nil {
		return textResult("Windows runtime did not return an app snapshot.", true)
	}
	s.rememberSnapshot(request.App, response.Snapshot)
	return response.Snapshot.result()
}

func (s *service) rememberSnapshot(query string, snapshot *appSnapshot) {
	keys := []string{query, snapshot.App.Name, snapshot.App.BundleIdentifier, strconv.Itoa(snapshot.App.PID)}
	for _, key := range keys {
		key = strings.ToLower(strings.TrimSpace(key))
		if key != "" {
			s.snapshots[key] = snapshot
		}
	}
}

func lookupElement(snapshot *appSnapshot, elementIndex string) (*elementRecord, error) {
	index, err := strconv.Atoi(elementIndex)
	if err != nil {
		return nil, fmt.Errorf("unknown element_index %q", elementIndex)
	}
	for _, record := range snapshot.Elements {
		if record.Index == index {
			copy := record
			return &copy, nil
		}
	}
	return nil, fmt.Errorf("unknown element_index %q", elementIndex)
}

func runPowerShell(request psRequest) (*psResponse, error) {
	if runtime.GOOS != "windows" {
		return nil, errors.New("Windows Computer Use runtime requires powershell.exe on Windows")
	}

	tempDir, err := os.MkdirTemp("", "codey-computer-use-*")
	if err != nil {
		return nil, err
	}
	defer os.RemoveAll(tempDir)

	scriptPath := filepath.Join(tempDir, "runtime.ps1")
	operationPath := filepath.Join(tempDir, "operation.json")
	if err := os.WriteFile(scriptPath, []byte(windowsRuntimeScript), 0o600); err != nil {
		return nil, err
	}
	operationData, err := json.Marshal(request)
	if err != nil {
		return nil, err
	}
	if err := os.WriteFile(operationPath, operationData, 0o600); err != nil {
		return nil, err
	}

	ctx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
	defer cancel()

	systemRoot := os.Getenv("SystemRoot")
	if !filepath.IsAbs(systemRoot) {
		return nil, errors.New("Cannot locate Windows PowerShell: SystemRoot must be an absolute path")
	}
	powershell := filepath.Join(systemRoot, "System32", "WindowsPowerShell", "v1.0", "powershell.exe")
	cmd := exec.CommandContext(ctx, powershell, "-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-File", scriptPath, operationPath)
	configureCommand(cmd)
	output, err := cmd.CombinedOutput()
	if ctx.Err() == context.DeadlineExceeded {
		return nil, errors.New("Windows runtime timed out after 30s")
	}
	if err != nil {
		text := strings.TrimSpace(string(output))
		if text == "" {
			text = err.Error()
		}
		return nil, fmt.Errorf("Windows runtime failed: %s", text)
	}

	var response psResponse
	if err := json.Unmarshal(output, &response); err != nil {
		return nil, fmt.Errorf("Windows runtime returned invalid JSON: %w: %s", err, strings.TrimSpace(string(output)))
	}
	return &response, nil
}

func requiredString(args map[string]any, key string) string {
	value, _ := args[key].(string)
	if key == "text" || key == "value" {
		return value
	}
	return strings.TrimSpace(value)
}

func (tool toolDefinition) validateArguments(args map[string]any) error {
	required, _ := tool.InputSchema["required"].([]string)
	for _, key := range required {
		if _, exists := args[key]; !exists {
			return fmt.Errorf("Missing required argument: %s", key)
		}
	}
	properties := tool.InputSchema["properties"].(map[string]any)
	for key, value := range args {
		schema, ok := properties[key].(map[string]any)
		if !ok || !matchesToolValue(value, schema) {
			return fmt.Errorf("Invalid argument: %s", key)
		}
	}
	return nil
}

func matchesToolValue(value any, schema map[string]any) bool {
	if variants, ok := schema["anyOf"].([]any); ok {
		for _, variant := range variants {
			if matchesToolValue(value, variant.(map[string]any)) {
				return true
			}
		}
		return false
	}
	if schema["type"] == "string" {
		text, ok := value.(string)
		if !ok {
			return false
		}
		if values, ok := schema["enum"].([]string); ok {
			for _, candidate := range values {
				if text == candidate {
					return true
				}
			}
			return false
		}
		return true
	}
	number, ok := value.(float64)
	if !ok || math.IsNaN(number) || math.IsInf(number, 0) || math.Abs(number) >= 9_007_199_254_740_992 {
		return false
	}
	if schema["type"] == "integer" && math.Trunc(number) != number {
		return false
	}
	if minimum, ok := schema["minimum"].(int); ok && number < float64(minimum) {
		return false
	}
	if maximum, ok := schema["maximum"].(int); ok && number > float64(maximum) {
		return false
	}
	return true
}

func optionalString(args map[string]any, key string) string {
	value, _ := args[key].(string)
	return value
}

func requiredElementIndex(args map[string]any) string {
	return strings.TrimSpace(optionalElementIndex(args))
}

func optionalElementIndex(args map[string]any) string {
	return elementIndexString(args["element_index"])
}

func elementIndexString(value any) string {
	switch value := value.(type) {
	case string:
		return value
	case json.Number:
		if integer, err := value.Int64(); err == nil {
			return strconv.FormatInt(integer, 10)
		}
		if float, err := value.Float64(); err == nil {
			return integerElementIndexFloat(float)
		}
	case float64:
		return integerElementIndexFloat(value)
	case int:
		return strconv.Itoa(value)
	case int64:
		return strconv.FormatInt(value, 10)
	}
	return ""
}

func integerElementIndexFloat(value float64) string {
	if math.IsNaN(value) || math.IsInf(value, 0) || math.Trunc(value) != value {
		return ""
	}
	return strconv.FormatInt(int64(value), 10)
}

func optionalFloat(args map[string]any, key string) *float64 {
	switch value := args[key].(type) {
	case float64:
		return &value
	case int:
		float := float64(value)
		return &float
	case json.Number:
		float, err := value.Float64()
		if err == nil {
			return &float
		}
	}
	return nil
}

func optionalTextLimit(args map[string]any, key string) (*textLimit, error) {
	value, ok := args[key]
	if !ok {
		return nil, nil
	}
	return textLimitFromValue(value, key)
}

func textLimitFromValue(value any, key string) (*textLimit, error) {
	if stringValue, ok := value.(string); ok {
		if strings.EqualFold(stringValue, "max") {
			return &textLimit{max: true}, nil
		}
		return nil, fmt.Errorf("%s must be a positive integer or max", key)
	}
	integer, err := positiveIntFromValue(value, key)
	if err != nil {
		return nil, fmt.Errorf("%s must be a positive integer or max", key)
	}
	return &textLimit{count: *integer}, nil
}

func optionalPositiveInt(args map[string]any, key string) (*int, error) {
	value, ok := args[key]
	if !ok {
		return nil, nil
	}
	return positiveIntFromValue(value, key)
}

func positiveIntFromValue(value any, key string) (*int, error) {
	switch typed := value.(type) {
	case int:
		return positiveIntFromInt64(int64(typed), key)
	case float64:
		if !isWholeNumber(typed) {
			return nil, fmt.Errorf("%s must be a positive integer", key)
		}
		return positiveIntFromFloat64(typed, key)
	case json.Number:
		integer, err := typed.Int64()
		if err != nil {
			return nil, fmt.Errorf("%s must be a positive integer", key)
		}
		return positiveIntFromInt64(integer, key)
	default:
		return nil, fmt.Errorf("%s must be a positive integer", key)
	}
}

func positiveIntFromFloat64(value float64, key string) (*int, error) {
	if !isWholeNumber(value) || value <= 0 || value > float64(maxInt()) {
		return nil, fmt.Errorf("%s must be a positive integer", key)
	}
	integer := int(value)
	return &integer, nil
}

func positiveIntFromInt64(value int64, key string) (*int, error) {
	if value <= 0 || value > int64(maxInt()) {
		return nil, fmt.Errorf("%s must be a positive integer", key)
	}
	integer := int(value)
	return &integer, nil
}

func isWholeNumber(value float64) bool {
	return !math.IsNaN(value) && !math.IsInf(value, 0) && math.Trunc(value) == value
}

func maxInt() int {
	return int(^uint(0) >> 1)
}

func intValue(value *float64, fallback int) int {
	if value == nil {
		return fallback
	}
	return int(*value)
}

func floatValue(value *float64, fallback float64) float64 {
	if value == nil {
		return fallback
	}
	return *value
}

func defaultString(value, fallback string) string {
	if strings.TrimSpace(value) == "" {
		return fallback
	}
	return value
}

func parseClickMethod(value string) (string, error) {
	normalized := strings.ToLower(strings.TrimSpace(value))
	if normalized == "" {
		return "auto", nil
	}
	for _, candidate := range clickMethodValues {
		if normalized == candidate {
			return normalized, nil
		}
	}
	return "", fmt.Errorf("Invalid click_method %q. Expected one of: %s", value, strings.Join(clickMethodValues, ", "))
}

var toolDefinitions = []toolDefinition{
	{
		Name:        "click",
		Description: "Click an element by index or pixel coordinates from screenshot.",
		Annotations: defaultAnnotations(),
		InputSchema: objectSchema(map[string]any{
			"app":           stringProperty("App name or bundle identifier"),
			"element_index": elementIndexProperty("Element index to click"),
			"x":             numberProperty("X coordinate in screenshot pixel coordinates"),
			"y":             numberProperty("Y coordinate in screenshot pixel coordinates"),
			"click_count":   integerProperty("Number of clicks. Defaults to 1"),
			"mouse_button":  enumStringProperty("Mouse button to click. Defaults to left.", []string{"left", "right", "middle"}),
			"click_method":  enumStringProperty("Click implementation: auto (default), accessibility, app_post, sky_click, or global. Accessibility requires element_index. Windows supports app_post through HWND messages and does not currently support sky_click or global.", clickMethodValues),
		}, []string{"app"}),
	},
	{
		Name:        "drag",
		Description: "Drag from one point to another using pixel coordinates.",
		Annotations: defaultAnnotations(),
		InputSchema: objectSchema(map[string]any{
			"app":    stringProperty("App name or bundle identifier"),
			"from_x": numberProperty("Start X coordinate"),
			"from_y": numberProperty("Start Y coordinate"),
			"to_x":   numberProperty("End X coordinate"),
			"to_y":   numberProperty("End Y coordinate"),
		}, []string{"app", "from_x", "from_y", "to_x", "to_y"}),
	},
	{
		Name:        "get_app_state",
		Description: "Get the state of an already running app's key window and return a screenshot and accessibility tree. Call this before acting and refresh after navigation.",
		Annotations: readOnlyAnnotations(),
		InputSchema: objectSchema(map[string]any{
			"app":            stringProperty("App name or bundle identifier"),
			"text_limit":     textLimitProperty("Maximum text characters to return. Use \"max\" for full text. Defaults to 500."),
			"max_tree_nodes": positiveIntegerProperty("Maximum accessibility tree nodes to render. Defaults to 1200."),
			"max_tree_depth": positiveIntegerProperty("Maximum accessibility tree depth to render. Defaults to 64."),
		}, []string{"app"}),
	},
	{
		Name:        "list_apps",
		Description: "List currently running apps with visible top-level windows.",
		Annotations: readOnlyAnnotations(),
		InputSchema: objectSchema(map[string]any{}, nil),
	},
	{
		Name:        "perform_secondary_action",
		Description: "Invoke a secondary accessibility action exposed by an element.",
		Annotations: defaultAnnotations(),
		InputSchema: objectSchema(map[string]any{
			"app":           stringProperty("App name or bundle identifier"),
			"element_index": elementIndexProperty("Element identifier"),
			"action":        stringProperty("Secondary accessibility action name"),
		}, []string{"app", "element_index", "action"}),
	},
	{
		Name:        "press_key",
		Description: "Press a key or key-combination on the keyboard, including modifier and navigation keys.\n  - This supports xdotool's `key` syntax.\n  - Examples: \"a\", \"Return\", \"Tab\", \"super+c\", \"Up\", \"KP_0\" (for the numpad 0).",
		Annotations: defaultAnnotations(),
		InputSchema: objectSchema(map[string]any{
			"app": stringProperty("App name or bundle identifier"),
			"key": stringProperty("Key or key-combination to press"),
		}, []string{"app", "key"}),
	},
	{
		Name:        "scroll",
		Description: "Scroll an element in a direction by a number of pages.",
		Annotations: defaultAnnotations(),
		InputSchema: objectSchema(map[string]any{
			"app":           stringProperty("App name or bundle identifier"),
			"direction":     stringProperty("Scroll direction: up, down, left, or right"),
			"element_index": elementIndexProperty("Element identifier"),
			"pages":         numberProperty("Number of pages to scroll. Fractional values are supported. Defaults to 1"),
		}, []string{"app", "element_index", "direction"}),
	},
	{
		Name:        "set_value",
		Description: "Set the value of a settable accessibility element.",
		Annotations: defaultAnnotations(),
		InputSchema: objectSchema(map[string]any{
			"app":           stringProperty("App name or bundle identifier"),
			"element_index": elementIndexProperty("Element identifier"),
			"value":         stringProperty("Value to assign"),
		}, []string{"app", "element_index", "value"}),
	},
	{
		Name:        "type_text",
		Description: "Type literal text using keyboard input.",
		Annotations: defaultAnnotations(),
		InputSchema: objectSchema(map[string]any{
			"app":  stringProperty("App name or bundle identifier"),
			"text": stringProperty("Literal text to type"),
		}, []string{"app", "text"}),
	},
}

var toolsByName = func() map[string]toolDefinition {
	tools := make(map[string]toolDefinition, len(toolDefinitions))
	for _, tool := range toolDefinitions {
		tools[tool.Name] = tool
	}
	return tools
}()

func objectSchema(properties map[string]any, required []string) map[string]any {
	schema := map[string]any{
		"type":                 "object",
		"properties":           properties,
		"additionalProperties": false,
	}
	if len(required) > 0 {
		schema["required"] = required
	}
	return schema
}

func defaultAnnotations() map[string]any {
	return map[string]any{"readOnlyHint": false, "destructiveHint": true, "openWorldHint": true}
}

func readOnlyAnnotations() map[string]any {
	return map[string]any{"destructiveHint": false, "idempotentHint": true, "openWorldHint": false, "readOnlyHint": true}
}

func stringProperty(description string) map[string]any {
	return map[string]any{"type": "string", "description": description}
}

func enumStringProperty(description string, values []string) map[string]any {
	property := stringProperty(description)
	property["enum"] = values
	return property
}

func numberProperty(description string) map[string]any {
	return map[string]any{"type": "number", "description": description}
}

func integerProperty(description string) map[string]any {
	return map[string]any{"type": "integer", "minimum": 1, "maximum": 3, "description": description}
}

func elementIndexProperty(description string) map[string]any {
	return map[string]any{"anyOf": []any{map[string]any{"type": "string"}, map[string]any{"type": "integer", "minimum": 0}}, "description": description}
}

func positiveIntegerProperty(description string) map[string]any {
	return map[string]any{"type": "integer", "minimum": 1, "description": description}
}

func textLimitProperty(description string) map[string]any {
	return map[string]any{
		"anyOf": []any{
			map[string]any{"type": "integer", "minimum": 1},
			map[string]any{"type": "string", "enum": []string{"max"}},
		},
		"description": description,
	}
}

func main() {
	if err := runMCP(os.Stdin, os.Stdout); err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
}

func runMCP(stdin io.Reader, stdout io.Writer) error {
	svc := newService()
	reader := bufio.NewReader(stdin)
	encoder := json.NewEncoder(stdout)
	for {
		var line []byte
		oversized := false
		var readErr error
		for {
			var part []byte
			part, readErr = reader.ReadSlice('\n')
			if len(line)+len(part) > 1_048_577 {
				oversized = true
			}
			if !oversized {
				line = append(line, part...)
			}
			if !errors.Is(readErr, bufio.ErrBufferFull) {
				break
			}
		}
		if readErr != nil && !errors.Is(readErr, io.EOF) {
			return readErr
		}
		var response map[string]any
		if oversized {
			response = jsonRPCError(nil, -32700, "Request exceeds 1 MiB")
		} else if len(bytes.TrimSpace(line)) > 0 {
			var object any
			if err := json.Unmarshal(line, &object); err != nil {
				response = jsonRPCError(nil, -32700, "Invalid JSON")
			} else if request, ok := object.(map[string]any); ok {
				response = handleMCPRequest(request, svc)
			} else {
				response = jsonRPCError(nil, -32600, "Invalid JSON-RPC request")
			}
		}
		if response != nil {
			if err := encoder.Encode(response); err != nil {
				return err
			}
		}
		if errors.Is(readErr, io.EOF) {
			return nil
		}
	}
}

func handleMCPRequest(request map[string]any, svc *service) map[string]any {
	method, _ := request["method"].(string)
	if request["jsonrpc"] != "2.0" || method == "" {
		return jsonRPCError(nil, -32600, "Invalid JSON-RPC request")
	}
	id, hasID := request["id"]
	if !hasID {
		return nil
	} // Notifications must never execute UI actions.
	switch id.(type) {
	case string, float64:
	default:
		return jsonRPCError(nil, -32600, "Invalid request ID")
	}
	params, _ := request["params"].(map[string]any)
	if _, exists := request["params"]; exists && params == nil {
		return jsonRPCError(id, -32602, "params must be an object")
	}
	switch method {
	case "initialize":
		return jsonRPCResult(id, map[string]any{
			"protocolVersion": "2025-03-26",
			"serverInfo": map[string]any{
				"name":    "codey-computer-use",
				"version": version,
			},
			"capabilities": map[string]any{"tools": map[string]any{"listChanged": false}},
			"instructions": serverInstructions,
		})
	case "ping":
		return jsonRPCResult(id, map[string]any{})
	case "tools/list":
		return jsonRPCResult(id, map[string]any{"tools": toolDefinitions})
	case "tools/call":
		name, _ := params["name"].(string)
		arguments, _ := params["arguments"].(map[string]any)
		if _, exists := params["arguments"]; exists && arguments == nil {
			return jsonRPCError(id, -32602, "arguments must be an object")
		}
		tool, found := toolsByName[name]
		if !found {
			return jsonRPCError(id, -32602, "Unknown tool")
		}
		if arguments == nil {
			arguments = map[string]any{}
		}
		return jsonRPCResult(id, svc.callTool(tool, arguments))
	default:
		return jsonRPCError(id, -32601, "Method not found")
	}
}

func jsonRPCResult(id any, result any) map[string]any {
	return map[string]any{"jsonrpc": "2.0", "id": id, "result": result}
}

func jsonRPCError(id any, code int, message string) map[string]any {
	return map[string]any{
		"jsonrpc": "2.0",
		"id":      id,
		"error":   map[string]any{"code": code, "message": message},
	}
}
