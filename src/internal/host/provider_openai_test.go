package host

import (
	"context"
	"encoding/json"
	"io"
	"log/slog"
	"net/http"
	"net/http/httptest"
	"os"
	"strings"
	"testing"

	"github.com/PromptPasture/jan-klod/internal/config"
)

const providerWasm = "../../../ext/provider-openai.wasm"

// loadProvider boots a host with provider-openai pointed at base-url, skipping
// if the wasm artifact has not been built (run `make ext`).
func loadProvider(t *testing.T, baseURL, apiKey, model string) *Extension {
	t.Helper()
	if _, err := os.Stat(providerWasm); err != nil {
		t.Skipf("provider-openai.wasm not built (%v); run `make ext`", err)
	}

	ctx := context.Background()
	logger := slog.New(slog.NewTextHandler(io.Discard, nil))
	h, err := New(ctx, logger)
	if err != nil {
		t.Fatalf("New: %v", err)
	}
	t.Cleanup(func() { _ = h.Close(ctx) })

	cfg := &config.Config{Extensions: map[string]config.ExtensionConfig{
		"provider-openai": {Enabled: true, Settings: map[string]any{
			"base-url": baseURL,
			"api-key":  apiKey,
			"model":    model,
		}},
	}}
	if err := h.LoadConfigured(ctx, probeExtDir, cfg, "0.1.0"); err != nil {
		t.Fatalf("LoadConfigured: %v", err)
	}
	ext, ok := h.Extension("provider-openai")
	if !ok {
		t.Fatal("provider-openai not registered")
	}
	return ext
}

type providerResp struct {
	Ok           bool   `json:"ok"`
	Error        string `json:"error"`
	Content      string `json:"content"`
	FinishReason string `json:"finish-reason"`
}

func TestProviderComplete(t *testing.T) {
	var gotAuth, gotModel string
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.URL.Path != "/chat/completions" {
			t.Errorf("unexpected path %s", r.URL.Path)
		}
		gotAuth = r.Header.Get("Authorization")
		body, _ := io.ReadAll(r.Body)
		var req struct {
			Model    string `json:"model"`
			Stream   bool   `json:"stream"`
			Messages []struct {
				Role    string `json:"role"`
				Content string `json:"content"`
			} `json:"messages"`
		}
		_ = json.Unmarshal(body, &req)
		gotModel = req.Model
		if req.Stream {
			t.Errorf("expected non-streaming request")
		}
		w.Header().Set("Content-Type", "application/json")
		_, _ = w.Write([]byte(`{"choices":[{"message":{"role":"assistant","content":"4"},"finish_reason":"stop"}]}`))
	}))
	defer srv.Close()

	ext := loadProvider(t, srv.URL, "sk-test", "gpt-4o-mini")

	req, _ := json.Marshal(map[string]any{
		"op":       "complete",
		"messages": []map[string]string{{"role": "user", "content": "2+2?"}},
	})
	out, err := ext.Call(context.Background(), req)
	if err != nil {
		t.Fatalf("Call: %v", err)
	}
	var resp providerResp
	if err := json.Unmarshal(out, &resp); err != nil {
		t.Fatalf("unmarshal %s: %v", out, err)
	}
	if !resp.Ok {
		t.Fatalf("complete failed: %s", resp.Error)
	}
	if resp.Content != "4" {
		t.Errorf("content: want 4, got %q", resp.Content)
	}
	if resp.FinishReason != "stop" {
		t.Errorf("finish-reason: want stop, got %q", resp.FinishReason)
	}
	if gotAuth != "Bearer sk-test" {
		t.Errorf("auth header: want Bearer sk-test, got %q", gotAuth)
	}
	if gotModel != "gpt-4o-mini" {
		t.Errorf("model: want gpt-4o-mini (from config default), got %q", gotModel)
	}
}

func TestProviderAuthFailed(t *testing.T) {
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		w.WriteHeader(http.StatusUnauthorized)
		_, _ = w.Write([]byte(`{"error":{"message":"invalid api key"}}`))
	}))
	defer srv.Close()

	ext := loadProvider(t, srv.URL, "bad", "gpt-4o-mini")

	req, _ := json.Marshal(map[string]any{
		"op":       "complete",
		"messages": []map[string]string{{"role": "user", "content": "hi"}},
	})
	out, err := ext.Call(context.Background(), req)
	if err != nil {
		t.Fatalf("Call: %v", err)
	}
	var resp providerResp
	if err := json.Unmarshal(out, &resp); err != nil {
		t.Fatalf("unmarshal: %v", err)
	}
	if resp.Ok || resp.Error != "auth-failed" {
		t.Errorf("want auth-failed, got ok=%v error=%q", resp.Ok, resp.Error)
	}
}

func TestProviderStartFailsWithoutBaseURL(t *testing.T) {
	if _, err := os.Stat(providerWasm); err != nil {
		t.Skipf("provider-openai.wasm not built (%v); run `make ext`", err)
	}
	ctx := context.Background()
	logger := slog.New(slog.NewTextHandler(io.Discard, nil))
	h, err := New(ctx, logger)
	if err != nil {
		t.Fatalf("New: %v", err)
	}
	t.Cleanup(func() { _ = h.Close(ctx) })

	cfg := &config.Config{Extensions: map[string]config.ExtensionConfig{
		"provider-openai": {Enabled: true, Settings: map[string]any{}},
	}}
	err = h.LoadConfigured(ctx, probeExtDir, cfg, "0.1.0")
	if err == nil || !strings.Contains(err.Error(), "base-url") {
		t.Errorf("want start failure mentioning base-url, got %v", err)
	}
}
