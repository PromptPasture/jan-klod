// Command jan-klod is the core runtime. It boots the WASM host, loads the
// extensions declared in jan-klod.yaml, and runs each through its lifecycle.
// As a temporary sign of life it exercises the store-memory extension when
// present, proving the host<->guest contract still works through the loader.
package main

import (
	"context"
	"encoding/json"
	"fmt"
	"log/slog"
	"os"

	"github.com/PromptPasture/jan-klod/internal/config"
	"github.com/PromptPasture/jan-klod/internal/host"
)

const (
	configPath = "jan-klod.yaml"
	extDir     = "ext"
	version    = "0.1.0"
)

func main() {
	if err := run(); err != nil {
		fmt.Fprintln(os.Stderr, "error:", err)
		os.Exit(1)
	}
}

func run() error {
	ctx := context.Background()
	logger := slog.New(slog.NewTextHandler(os.Stderr, nil))

	cfg, err := config.Load(configPath)
	if err != nil {
		return err
	}

	h, err := host.New(ctx, logger)
	if err != nil {
		return err
	}
	defer func() { _ = h.Close(ctx) }()

	if err := h.LoadConfigured(ctx, extDir, cfg, version); err != nil {
		return err
	}

	if ext, ok := h.Extension("store-memory"); ok {
		if err := storeSmokeTest(ctx, ext); err != nil {
			return err
		}
	}
	return nil
}

// storeSmokeTest exercises the store-memory contract end to end.
func storeSmokeTest(ctx context.Context, ext *host.Extension) error {
	steps := []storeRequest{
		{Op: "set", Namespace: "demo", Key: "greeting", Value: "hello"},
		{Op: "set", Namespace: "demo", Key: "farewell", Value: "goodbye"},
		{Op: "get", Namespace: "demo", Key: "greeting"},
		{Op: "list-keys", Namespace: "demo"},
		{Op: "recent", Namespace: "demo", Limit: 10},
		{Op: "get", Namespace: "demo", Key: "missing"},
	}

	for _, req := range steps {
		resp, err := callStore(ctx, ext, req)
		if err != nil {
			return err
		}
		fmt.Printf("%-9s %-9s -> %s\n", req.Op, req.Key, resp)
	}
	return nil
}

type storeRequest struct {
	Op        string `json:"op"`
	Namespace string `json:"namespace,omitempty"`
	Key       string `json:"key,omitempty"`
	Value     string `json:"value,omitempty"`
	Limit     uint32 `json:"limit,omitempty"`
}

func callStore(ctx context.Context, ext *host.Extension, req storeRequest) (string, error) {
	payload, err := json.Marshal(req)
	if err != nil {
		return "", fmt.Errorf("marshal request: %w", err)
	}
	out, err := ext.Call(ctx, payload)
	if err != nil {
		return "", err
	}
	return string(out), nil
}
