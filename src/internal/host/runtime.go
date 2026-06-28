// Package host embeds the wazero WASM runtime and loads sandboxed extensions.
package host

import (
	"context"
	"fmt"
	"log/slog"
	"net/http"

	"github.com/tetratelabs/wazero"
	"github.com/tetratelabs/wazero/api"
	"github.com/tetratelabs/wazero/imports/wasi_snapshot_preview1"
)

// Host owns the WASM runtime and the host-provided capability modules that
// every extension may import.
type Host struct {
	runtime    wazero.Runtime
	logger     *slog.Logger
	exts       map[string]*Extension
	configs    map[string]map[string]any
	httpClient *http.Client
}

// New creates a runtime with WASI and the "jan-klod" host module registered.
func New(ctx context.Context, logger *slog.Logger) (*Host, error) {
	rt := wazero.NewRuntime(ctx)
	if _, err := wasi_snapshot_preview1.Instantiate(ctx, rt); err != nil {
		return nil, fmt.Errorf("instantiate wasi: %w", err)
	}

	h := &Host{
		runtime:    rt,
		logger:     logger,
		exts:       map[string]*Extension{},
		configs:    map[string]map[string]any{},
		httpClient: &http.Client{},
	}
	if err := h.registerHostModule(ctx); err != nil {
		return nil, err
	}
	return h, nil
}

// registerHostModule exposes the host-provided interfaces extensions import:
// host-log, host-config, and host-http (see wit/host-*.wit). Functions that
// return data write the result into the caller's linear memory via the guest's
// own alloc export and return Pack(ptr, len); the guest reads and frees it.
func (h *Host) registerHostModule(ctx context.Context) error {
	_, err := h.runtime.NewHostModuleBuilder("jan-klod").
		NewFunctionBuilder().
		WithFunc(func(_ context.Context, m api.Module, level, ptr, size uint32) {
			msg := ""
			if size > 0 {
				if b, ok := m.Memory().Read(ptr, size); ok {
					msg = string(b)
				}
			}
			h.forwardLog(level, msg)
		}).
		Export("log").
		NewFunctionBuilder().
		WithFunc(func(ctx context.Context, m api.Module, keyPtr, keyLen uint32) uint64 {
			key := readGuestString(m, keyPtr, keyLen)
			return h.returnJSON(ctx, m, h.configGet(m.Name(), key))
		}).
		Export("config_get").
		NewFunctionBuilder().
		WithFunc(func(ctx context.Context, m api.Module, reqPtr, reqLen uint32) uint64 {
			req, _ := m.Memory().Read(reqPtr, reqLen)
			return h.returnJSON(ctx, m, h.httpFetch(ctx, req))
		}).
		Export("http_fetch").
		Instantiate(ctx)
	if err != nil {
		return fmt.Errorf("register host module: %w", err)
	}
	return nil
}

// forwardLog maps the guest log level (0=debug..3=error) to the core logger.
func (h *Host) forwardLog(level uint32, msg string) {
	switch level {
	case 0:
		h.logger.Debug(msg, "src", "ext")
	case 1:
		h.logger.Info(msg, "src", "ext")
	case 2:
		h.logger.Warn(msg, "src", "ext")
	default:
		h.logger.Error(msg, "src", "ext")
	}
}

// Close stops every loaded extension, then releases the runtime and all
// instantiated modules.
func (h *Host) Close(ctx context.Context) error {
	h.stopAll(ctx)
	return h.runtime.Close(ctx)
}
