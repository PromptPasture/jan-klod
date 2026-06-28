package host

import (
	"context"
	"fmt"
	"path/filepath"

	"github.com/PromptPasture/jan-klod/internal/config"
)

// LoadConfigured loads every enabled extension declared in cfg from extDir,
// running each through its lifecycle (init then start) and recording it in the
// host registry. Disabled extensions are skipped. The .wasm file is expected at
// extDir/<name>.wasm.
func (h *Host) LoadConfigured(ctx context.Context, extDir string, cfg *config.Config, version string) error {
	for name, ec := range cfg.Extensions {
		if !ec.Enabled {
			h.logger.Info("extension disabled, skipping", "name", name)
			continue
		}

		// Register the extension's config section before load so init/start
		// hooks can read it through host-config.
		h.configs[name] = ec.Settings

		path := filepath.Join(extDir, name+".wasm")
		ext, err := h.Load(ctx, name, path)
		if err != nil {
			return fmt.Errorf("load %s: %w", name, err)
		}

		if err := ext.Init(ctx, version); err != nil {
			return fmt.Errorf("init %s: %w", name, err)
		}
		if err := ext.Start(ctx); err != nil {
			return fmt.Errorf("start %s: %w", name, err)
		}

		status, err := ext.Health(ctx)
		if err != nil {
			return fmt.Errorf("health %s: %w", name, err)
		}
		h.logger.Info("extension started", "name", name, "health", status)
	}
	return nil
}

// Extension returns a loaded extension by name, or false if not present.
func (h *Host) Extension(name string) (*Extension, bool) {
	ext, ok := h.exts[name]
	return ext, ok
}

// stopAll runs the lifecycle stop hook on every loaded extension. Errors are
// logged but never abort shutdown.
func (h *Host) stopAll(ctx context.Context) {
	for name, ext := range h.exts {
		if err := ext.Stop(ctx); err != nil {
			h.logger.Warn("extension stop failed", "name", name, "err", err)
		}
	}
}
