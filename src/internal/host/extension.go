package host

import (
	"context"
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
	return ext, nil
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
