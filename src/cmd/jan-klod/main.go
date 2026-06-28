// Command jan-klod is the core runtime. For this MVP slice it boots the WASM
// host, loads the store-memory extension, and runs a roundtrip smoke test that
// proves the host<->guest contract works end to end.
package main

import (
	"context"
	"encoding/json"
	"fmt"
	"log/slog"
	"os"

	"github.com/PromptPasture/jan-klod/internal/host"
)

const extPath = "ext/store-memory.wasm"

func main() {
	if err := run(); err != nil {
		fmt.Fprintln(os.Stderr, "error:", err)
		os.Exit(1)
	}
}

func run() error {
	ctx := context.Background()
	logger := slog.New(slog.NewTextHandler(os.Stderr, nil))

	h, err := host.New(ctx, logger)
	if err != nil {
		return err
	}
	defer func() { _ = h.Close(ctx) }()

	ext, err := h.Load(ctx, "store-memory", extPath)
	if err != nil {
		return err
	}
	logger.Info("extension loaded", "name", ext.Name())

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
