// Package host embeds the wazero WASM runtime and loads sandboxed extensions.
package host

import (
	"context"
	"fmt"
	"log/slog"

	"github.com/tetratelabs/wazero"
	"github.com/tetratelabs/wazero/api"
	"github.com/tetratelabs/wazero/imports/wasi_snapshot_preview1"
)

// Host owns the WASM runtime and the host-provided capability modules that
// every extension may import.
type Host struct {
	runtime wazero.Runtime
	logger  *slog.Logger
}

// New creates a runtime with WASI and the "jan-klod" host module registered.
func New(ctx context.Context, logger *slog.Logger) (*Host, error) {
	rt := wazero.NewRuntime(ctx)
	if _, err := wasi_snapshot_preview1.Instantiate(ctx, rt); err != nil {
		return nil, fmt.Errorf("instantiate wasi: %w", err)
	}

	h := &Host{runtime: rt, logger: logger}
	if err := h.registerHostModule(ctx); err != nil {
		return nil, err
	}
	return h, nil
}

// registerHostModule exposes the host-provided interfaces extensions import.
// For this MVP slice only host-log is implemented (see wit/host-log.wit).
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

// Close releases the runtime and all instantiated modules.
func (h *Host) Close(ctx context.Context) error {
	return h.runtime.Close(ctx)
}
