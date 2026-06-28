package host

import (
	"context"
	"encoding/json"
	"io"
	"log/slog"
	"net/http"
	"net/http/httptest"
	"os"
	"testing"

	"github.com/PromptPasture/jan-klod/internal/config"
)

const probeWasm = "../../../ext/probe-host.wasm"
const probeExtDir = "../../../ext"

// loadProbe boots a host with the probe-host extension and the given config
// section, skipping if the wasm artifact has not been built (run `make ext`).
func loadProbe(t *testing.T, settings map[string]any) (*Host, *Extension) {
	t.Helper()
	if _, err := os.Stat(probeWasm); err != nil {
		t.Skipf("probe-host.wasm not built (%v); run `make ext`", err)
	}

	ctx := context.Background()
	logger := slog.New(slog.NewTextHandler(io.Discard, nil))
	h, err := New(ctx, logger)
	if err != nil {
		t.Fatalf("New: %v", err)
	}
	t.Cleanup(func() { _ = h.Close(ctx) })

	cfg := &config.Config{Extensions: map[string]config.ExtensionConfig{
		"probe-host": {Enabled: true, Settings: settings},
	}}
	if err := h.LoadConfigured(ctx, probeExtDir, cfg, "0.1.0"); err != nil {
		t.Fatalf("LoadConfigured: %v", err)
	}
	ext, ok := h.Extension("probe-host")
	if !ok {
		t.Fatal("probe-host not registered")
	}
	return h, ext
}

func TestHostConfig(t *testing.T) {
	_, ext := loadProbe(t, map[string]any{"greeting": "hi", "model": "gpt-4o-mini"})

	out, err := ext.Call(context.Background(), []byte(`{"op":"config","key":"greeting"}`))
	if err != nil {
		t.Fatalf("Call: %v", err)
	}
	var resp struct {
		Ok    bool            `json:"ok"`
		Value json.RawMessage `json:"value"`
		Error string          `json:"error"`
	}
	if err := json.Unmarshal(out, &resp); err != nil {
		t.Fatalf("unmarshal %s: %v", out, err)
	}
	if !resp.Ok {
		t.Fatalf("config get failed: %s", resp.Error)
	}
	if string(resp.Value) != `"hi"` {
		t.Errorf("value: want \"hi\", got %s", resp.Value)
	}
}

func TestHostConfigMissingKey(t *testing.T) {
	_, ext := loadProbe(t, map[string]any{})

	out, err := ext.Call(context.Background(), []byte(`{"op":"config","key":"nope"}`))
	if err != nil {
		t.Fatalf("Call: %v", err)
	}
	var resp struct {
		Ok    bool   `json:"ok"`
		Error string `json:"error"`
	}
	if err := json.Unmarshal(out, &resp); err != nil {
		t.Fatalf("unmarshal: %v", err)
	}
	if resp.Ok || resp.Error != "key-not-found" {
		t.Errorf("want key-not-found error, got ok=%v error=%q", resp.Ok, resp.Error)
	}
}

func TestHostHTTPFetch(t *testing.T) {
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		body, _ := io.ReadAll(r.Body)
		w.WriteHeader(http.StatusOK)
		_, _ = w.Write([]byte("method=" + r.Method + " echo=" + string(body)))
	}))
	defer srv.Close()

	_, ext := loadProbe(t, map[string]any{})

	req, _ := json.Marshal(map[string]string{
		"op":     "fetch",
		"method": "POST",
		"url":    srv.URL,
		"body":   "ping",
	})
	out, err := ext.Call(context.Background(), req)
	if err != nil {
		t.Fatalf("Call: %v", err)
	}
	var resp struct {
		Ok     bool   `json:"ok"`
		Status int    `json:"status"`
		Body   string `json:"body"`
		Error  string `json:"error"`
	}
	if err := json.Unmarshal(out, &resp); err != nil {
		t.Fatalf("unmarshal %s: %v", out, err)
	}
	if !resp.Ok {
		t.Fatalf("fetch failed: %s", resp.Error)
	}
	if resp.Status != http.StatusOK {
		t.Errorf("status: want 200, got %d", resp.Status)
	}
	if resp.Body != "method=POST echo=ping" {
		t.Errorf("body round-trip wrong: %q", resp.Body)
	}
}

func TestHostHTTPConnectionFailed(t *testing.T) {
	_, ext := loadProbe(t, map[string]any{})

	// Closed local port — immediate connection-refused, no slow timeout.
	req, _ := json.Marshal(map[string]string{
		"op":     "fetch",
		"method": "GET",
		"url":    "http://127.0.0.1:1/",
	})
	out, err := ext.Call(context.Background(), req)
	if err != nil {
		t.Fatalf("Call: %v", err)
	}
	var resp struct {
		Ok    bool   `json:"ok"`
		Error string `json:"error"`
	}
	if err := json.Unmarshal(out, &resp); err != nil {
		t.Fatalf("unmarshal: %v", err)
	}
	if resp.Ok {
		t.Errorf("want transport failure, got ok=true")
	}
}
