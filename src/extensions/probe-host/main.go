//go:build wasip1

// probe-host is a test-fixture WASM extension that exercises the host-provided
// interfaces (host-log, host-config, host-http) across the JSON ABI. It is not
// shipped — it validates that the host functions round-trip correctly.
//
// ABI (core WASM module, JSON over linear memory):
//
//	alloc(size u32) -> ptr u32
//	free(ptr u32)
//	invoke(ptr u32, len u32) -> u64   // (resultPtr<<32 | resultLen)
//
// Imported from the host module "jan-klod":
//
//	log(level u32, ptr u32, len u32)
//	config_get(keyPtr u32, keyLen u32) -> u64   // (resultPtr<<32 | resultLen)
//	http_fetch(reqPtr u32, reqLen u32) -> u64   // (resultPtr<<32 | resultLen)
package main

import (
	"encoding/base64"
	"encoding/json"
	"unsafe"
)

func main() {}

var pinned = make(map[uint32][]byte)

//go:wasmexport alloc
func alloc(size uint32) uint32 {
	if size == 0 {
		size = 1
	}
	buf := make([]byte, size)
	ptr := uint32(uintptr(unsafe.Pointer(&buf[0])))
	pinned[ptr] = buf
	return ptr
}

//go:wasmexport free
func free(ptr uint32) {
	delete(pinned, ptr)
}

//go:wasmexport invoke
func invoke(ptr, size uint32) uint64 {
	in := unsafe.Slice((*byte)(unsafe.Pointer(uintptr(ptr))), size)
	out := dispatch(in)

	outPtr := alloc(uint32(len(out)))
	copy(pinned[outPtr], out)
	return uint64(outPtr)<<32 | uint64(len(out))
}

//go:wasmimport jan-klod log
func hostLog(level, ptr, size uint32)

//go:wasmimport jan-klod config_get
func hostConfigGet(keyPtr, keyLen uint32) uint64

//go:wasmimport jan-klod http_fetch
func hostHTTPFetch(reqPtr, reqLen uint32) uint64

func logInfo(msg string) {
	b := []byte(msg)
	var ptr uint32
	if len(b) > 0 {
		ptr = uint32(uintptr(unsafe.Pointer(&b[0])))
	}
	hostLog(1, ptr, uint32(len(b)))
}

// callHost invokes a host function that returns Pack(ptr,len) of bytes the host
// wrote into our linear memory, copies them out, and frees the buffer.
func callHost(fn func(uint32, uint32) uint64, payload []byte) []byte {
	var ptr uint32
	if len(payload) > 0 {
		ptr = uint32(uintptr(unsafe.Pointer(&payload[0])))
	}
	packed := fn(ptr, uint32(len(payload)))
	outPtr := uint32(packed >> 32)
	outLen := uint32(packed)
	if outLen == 0 {
		return nil
	}
	src := unsafe.Slice((*byte)(unsafe.Pointer(uintptr(outPtr))), outLen)
	out := make([]byte, outLen)
	copy(out, src)
	free(outPtr)
	return out
}

type request struct {
	Op     string `json:"op"`
	Key    string `json:"key"`
	Method string `json:"method"`
	URL    string `json:"url"`
	Body   string `json:"body"`
}

type fetchResult struct {
	Ok     bool   `json:"ok"`
	Status int    `json:"status,omitempty"`
	Body   string `json:"body,omitempty"`
	Error  string `json:"error,omitempty"`
}

func dispatch(in []byte) []byte {
	var req request
	if err := json.Unmarshal(in, &req); err != nil {
		return []byte(`{"ok":false,"error":"serialization"}`)
	}
	logInfo("probe-host op=" + req.Op)

	switch req.Op {
	case "lifecycle.init", "lifecycle.start", "lifecycle.stop":
		return []byte(`{"ok":true}`)
	case "lifecycle.health":
		return []byte(`{"ok":true,"status":"up"}`)
	case "config":
		// Pass the host-config result through unchanged.
		out := callHost(hostConfigGet, []byte(req.Key))
		if out == nil {
			return []byte(`{"ok":false,"error":"backend"}`)
		}
		return out
	case "fetch":
		return doFetch(req)
	default:
		return []byte(`{"ok":false,"error":"backend"}`)
	}
}

func doFetch(req request) []byte {
	hostReq := map[string]any{"method": req.Method, "url": req.URL}
	if req.Body != "" {
		hostReq["body"] = base64.StdEncoding.EncodeToString([]byte(req.Body))
	}
	reqBytes, _ := json.Marshal(hostReq)

	out := callHost(hostHTTPFetch, reqBytes)
	if out == nil {
		return []byte(`{"ok":false,"error":"backend"}`)
	}

	// Decode the host envelope and surface the body as plain text so callers
	// can read it directly.
	var hostResp struct {
		Ok     bool   `json:"ok"`
		Status int    `json:"status"`
		Body   string `json:"body"`
		Error  string `json:"error"`
	}
	if err := json.Unmarshal(out, &hostResp); err != nil {
		return []byte(`{"ok":false,"error":"serialization"}`)
	}
	if !hostResp.Ok {
		b, _ := json.Marshal(fetchResult{Error: hostResp.Error})
		return b
	}
	decoded, err := base64.StdEncoding.DecodeString(hostResp.Body)
	if err != nil {
		return []byte(`{"ok":false,"error":"serialization"}`)
	}
	b, _ := json.Marshal(fetchResult{Ok: true, Status: hostResp.Status, Body: string(decoded)})
	return b
}
