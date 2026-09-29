package main

import (
	"bytes"
	"encoding/json"
	"strings"
	"testing"
)

func TestMCPRecoversAfterMalformedAndOversizedLines(t *testing.T) {
	input := "not-json\n[]\n" + strings.Repeat("x", 1_052_672) + "\n" +
		`{"jsonrpc":"2.0","method":"tools/call","params":{"name":"list_apps"}}` + "\n" +
		`{"jsonrpc":"2.0","id":1,"method":"ping"}`
	var output bytes.Buffer
	if err := runMCP(strings.NewReader(input), &output); err != nil {
		t.Fatal(err)
	}
	lines := strings.Split(strings.TrimSpace(output.String()), "\n")
	if len(lines) != 4 {
		t.Fatalf("expected 4 responses, got %d", len(lines))
	}
	var reply map[string]any
	if err := json.Unmarshal([]byte(lines[3]), &reply); err != nil {
		t.Fatal(err)
	}
	if reply["id"] != float64(1) || reply["result"] == nil {
		t.Fatal(reply)
	}
}

func TestActionValidationAndLiteralText(t *testing.T) {
	for _, count := range []any{true, 1e30, -1.0, 1.5, "2"} {
		if err := toolsByName["click"].validateArguments(map[string]any{"app": "unused", "click_count": count}); err == nil {
			t.Fatalf("accepted invalid click_count: %v", count)
		}
	}
	if err := toolsByName["drag"].validateArguments(map[string]any{"app": "unused"}); err == nil {
		t.Fatal("accepted missing coordinates")
	}
	text := "  中文\n\t  "
	for _, key := range []string{"text", "value"} {
		if got := requiredString(map[string]any{key: text}, key); got != text {
			t.Fatalf("text changed: %q", got)
		}
	}
}

func TestMCPToolDispatchRejectsInvalidArgumentsBeforeActions(t *testing.T) {
	svc := newService()
	for _, tool := range toolDefinitions {
		t.Run(tool.Name, func(t *testing.T) {
			reply := handleMCPRequest(map[string]any{
				"jsonrpc": "2.0", "id": "validation", "method": "tools/call",
				"params": map[string]any{"name": tool.Name, "arguments": map[string]any{"unexpected": true}},
			}, svc)
			result, ok := reply["result"].(toolCallResult)
			if !ok || !result.IsError || len(result.Content) != 1 || result.Content[0].Text == "" {
				t.Fatalf("expected a tool validation error, got %#v", reply)
			}
		})
	}
	reply := handleMCPRequest(map[string]any{
		"jsonrpc": "2.0", "id": "unknown", "method": "tools/call",
		"params": map[string]any{"name": "unknown"},
	}, svc)
	if rpcError, ok := reply["error"].(map[string]any); !ok || rpcError["code"] != -32602 {
		t.Fatalf("expected an unknown-tool protocol error, got %#v", reply)
	}
}
