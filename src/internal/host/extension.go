package host

import (
	"context"
	"encoding/json"
	"fmt"
	"os"

	"github.com/PromptPasture/jan-klod/internal/abi"
	"github.com/tetratelabs/wazero"
	"github.com/tetratelabs/wazero/api"
)

// Extension is a loaded WASM extension instance.
type Extension struct {
	name   string
	module api.Module
	alloc  api.Function
	free   api.Function
	invoke api.Function
}

// Name returns the extension's registered name.
func (e *Extension) Name() string { return e.name }

// Load reads, compiles, and instantiates a WASM extension from path.
func (h *Host) Load(ctx context.Context, name, path string) (*Extension, error) {
	wasm, err := os.ReadFile(path)
	if err != nil {
		return nil, fmt.Errorf("read %s: %w", path, err)
	}

	compiled, err := h.runtime.CompileModule(ctx, wasm)
	if err != nil {
		return nil, fmt.Errorf("compile %s: %w", name, err)
	}

	// Go's wasip1 c-shared output is a reactor: it exposes _initialize rather
	// than _start, so wazero must be told to run that on instantiation.
	config := wazero.NewModuleConfig().
		WithName(name).
		WithStartFunctions("_initialize").
		WithStdout(os.Stdout).
		WithStderr(os.Stderr).
		// wazero defaults to a fixed fake clock for determinism; extensions
		// need the real wall clock for entry timestamps.
		WithSysWalltime().
		WithSysNanotime()

	mod, err := h.runtime.InstantiateModule(ctx, compiled, config)
	if err != nil {
		return nil, fmt.Errorf("instantiate %s: %w", name, err)
	}

	ext := &Extension{
		name:   name,
		module: mod,
		alloc:  mod.ExportedFunction("alloc"),
		free:   mod.ExportedFunction("free"),
		invoke: mod.ExportedFunction("invoke"),
	}
	if ext.alloc == nil || ext.free == nil || ext.invoke == nil {
		return nil, fmt.Errorf("%s: missing required exports (alloc/free/invoke)", name)
	}
	h.exts[name] = ext
	return ext, nil
}

// Lifecycle ops are reserved invoke requests that map to the
// extension-lifecycle WIT interface (see wit/extension-lifecycle.wit). They
// share the single invoke entry point so every guest keeps the same three
// exports (alloc/free/invoke).
type lifecycleRequest struct {
	Op      string `json:"op"`
	ID      string `json:"id,omitempty"`
	Version string `json:"version,omitempty"`
}

type lifecycleResponse struct {
	Ok     bool   `json:"ok"`
	Error  string `json:"error,omitempty"`
	Status string `json:"status,omitempty"`
}

// Init runs the extension's lifecycle init hook with its host-injected identity.
func (e *Extension) Init(ctx context.Context, version string) error {
	return e.lifecycle(ctx, lifecycleRequest{Op: "lifecycle.init", ID: e.name, Version: version})
}

// Start runs the extension's lifecycle start hook.
func (e *Extension) Start(ctx context.Context) error {
	return e.lifecycle(ctx, lifecycleRequest{Op: "lifecycle.start"})
}

// Stop runs the extension's lifecycle stop hook. Errors are best-effort.
func (e *Extension) Stop(ctx context.Context) error {
	return e.lifecycle(ctx, lifecycleRequest{Op: "lifecycle.stop"})
}

// Health polls the extension's reported liveness ("up"|"degraded"|"down").
func (e *Extension) Health(ctx context.Context) (string, error) {
	resp, err := e.callLifecycle(ctx, lifecycleRequest{Op: "lifecycle.health"})
	if err != nil {
		return "down", err
	}
	if resp.Status == "" {
		return "up", nil
	}
	return resp.Status, nil
}

func (e *Extension) lifecycle(ctx context.Context, req lifecycleRequest) error {
	_, err := e.callLifecycle(ctx, req)
	return err
}

func (e *Extension) callLifecycle(ctx context.Context, req lifecycleRequest) (lifecycleResponse, error) {
	payload, err := json.Marshal(req)
	if err != nil {
		return lifecycleResponse{}, fmt.Errorf("%s %s: marshal: %w", e.name, req.Op, err)
	}
	out, err := e.Call(ctx, payload)
	if err != nil {
		return lifecycleResponse{}, fmt.Errorf("%s %s: %w", e.name, req.Op, err)
	}
	var resp lifecycleResponse
	if err := json.Unmarshal(out, &resp); err != nil {
		return lifecycleResponse{}, fmt.Errorf("%s %s: unmarshal: %w", e.name, req.Op, err)
	}
	if !resp.Ok {
		return resp, fmt.Errorf("%s %s: %s", e.name, req.Op, resp.Error)
	}
	return resp, nil
}

// Call sends a request payload to the extension and returns its response bytes.
// It copies the request into guest memory, invokes the extension, copies the
// response out, and frees both guest buffers.
func (e *Extension) Call(ctx context.Context, req []byte) ([]byte, error) {
	allocRes, err := e.alloc.Call(ctx, uint64(len(req)))
	if err != nil {
		return nil, fmt.Errorf("%s alloc: %w", e.name, err)
	}
	inPtr := uint32(allocRes[0])
	defer func() { _, _ = e.free.Call(ctx, uint64(inPtr)) }()

	if !e.module.Memory().Write(inPtr, req) {
		return nil, fmt.Errorf("%s: request write out of range", e.name)
	}

	invRes, err := e.invoke.Call(ctx, uint64(inPtr), uint64(len(req)))
	if err != nil {
		return nil, fmt.Errorf("%s invoke: %w", e.name, err)
	}

	outPtr, outLen := abi.Unpack(invRes[0])
	out, ok := e.module.Memory().Read(outPtr, outLen)
	if !ok {
		return nil, fmt.Errorf("%s: response read out of range", e.name)
	}

	result := make([]byte, len(out))
	copy(result, out)
	_, _ = e.free.Call(ctx, uint64(outPtr))
	return result, nil
}
