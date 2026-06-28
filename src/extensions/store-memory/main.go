//go:build wasip1

// store-memory is a WASM extension implementing the memory-store contract
// (see wit/memory-store.wit) with an in-memory map. It exists to validate the
// host<->guest ABI roundtrip end to end; it is not a persistent backend.
//
// ABI (core WASM module, JSON over linear memory):
//
//	alloc(size u32) -> ptr u32        host asks guest to reserve `size` bytes
//	free(ptr u32)                     host releases a guest buffer
//	invoke(ptr u32, len u32) -> u64   process JSON request; returns
//	                                  (resultPtr<<32 | resultLen)
//
// Imported from the host module "jan-klod":
//
//	log(level u32, ptr u32, len u32)  structured log forwarded to core
package main

import (
	"encoding/json"
	"sort"
	"strconv"
	"strings"
	"time"
	"unsafe"
)

func main() {}

// pinned keeps allocated buffers reachable so the Go GC does not reclaim memory
// whose raw linear-memory offset we have handed to the host.
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

func logInfo(msg string) {
	b := []byte(msg)
	var ptr uint32
	if len(b) > 0 {
		ptr = uint32(uintptr(unsafe.Pointer(&b[0])))
	}
	hostLog(1, ptr, uint32(len(b)))
}

type request struct {
	Op        string `json:"op"`
	Namespace string `json:"namespace"`
	Key       string `json:"key"`
	Value     string `json:"value"`
	Limit     uint32 `json:"limit"`
	Query     string `json:"query"`
}

type entry struct {
	ID        string `json:"id"`
	Namespace string `json:"namespace"`
	Key       string `json:"key"`
	Value     string `json:"value"`
	CreatedAt int64  `json:"created-at"`
	UpdatedAt int64  `json:"updated-at"`
}

type response struct {
	Ok      bool    `json:"ok"`
	Error   string  `json:"error,omitempty"`
	Status  string  `json:"status,omitempty"`
	Entry   *entry  `json:"entry,omitempty"`
	Entries []entry `json:"entries,omitempty"`
}

var (
	data   = map[string]map[string]entry{}
	nextID int64
)

func dispatch(in []byte) []byte {
	var req request
	if err := json.Unmarshal(in, &req); err != nil {
		return reply(response{Error: "serialization"})
	}
	logInfo("store-memory op=" + req.Op + " ns=" + req.Namespace)

	switch req.Op {
	case "lifecycle.init", "lifecycle.start":
		return reply(response{Ok: true})
	case "lifecycle.stop":
		data = map[string]map[string]entry{}
		return reply(response{Ok: true})
	case "lifecycle.health":
		return reply(response{Ok: true, Status: "up"})
	case "set":
		return reply(opSet(req))
	case "get":
		return reply(opGet(req))
	case "delete":
		return reply(opDelete(req))
	case "list-keys":
		return reply(opList(req))
	case "recent":
		return reply(opRecent(req))
	case "search":
		return reply(opSearch(req))
	case "purge-namespace":
		return reply(opPurge(req))
	default:
		return reply(response{Error: "backend"})
	}
}

func reply(r response) []byte {
	b, err := json.Marshal(r)
	if err != nil {
		return []byte(`{"ok":false,"error":"serialization"}`)
	}
	return b
}

func opSet(req request) response {
	ns := data[req.Namespace]
	if ns == nil {
		ns = map[string]entry{}
		data[req.Namespace] = ns
	}
	now := time.Now().Unix()
	e, exists := ns[req.Key]
	if exists {
		e.Value = req.Value
		e.UpdatedAt = now
	} else {
		nextID++
		e = entry{
			ID:        strconv.FormatInt(nextID, 10),
			Namespace: req.Namespace,
			Key:       req.Key,
			Value:     req.Value,
			CreatedAt: now,
			UpdatedAt: now,
		}
	}
	ns[req.Key] = e
	return response{Ok: true, Entry: &e}
}

func opGet(req request) response {
	if ns := data[req.Namespace]; ns != nil {
		if e, ok := ns[req.Key]; ok {
			return response{Ok: true, Entry: &e}
		}
	}
	return response{Error: "not-found"}
}

func opDelete(req request) response {
	if ns := data[req.Namespace]; ns != nil {
		delete(ns, req.Key)
	}
	return response{Ok: true}
}

func opList(req request) response {
	var out []entry
	for _, e := range data[req.Namespace] {
		e.Value = "" // list omits the payload
		out = append(out, e)
	}
	sort.Slice(out, func(i, j int) bool { return out[i].Key < out[j].Key })
	return response{Ok: true, Entries: out}
}

func opRecent(req request) response {
	var out []entry
	for _, e := range data[req.Namespace] {
		out = append(out, e)
	}
	sort.Slice(out, func(i, j int) bool { return out[i].UpdatedAt > out[j].UpdatedAt })
	out = limit(out, req.Limit)
	return response{Ok: true, Entries: out}
}

func opSearch(req request) response {
	var out []entry
	for _, e := range data[req.Namespace] {
		if strings.Contains(e.Value, req.Query) {
			out = append(out, e)
		}
	}
	sort.Slice(out, func(i, j int) bool { return out[i].Key < out[j].Key })
	out = limit(out, req.Limit)
	return response{Ok: true, Entries: out}
}

func opPurge(req request) response {
	delete(data, req.Namespace)
	return response{Ok: true}
}

func limit(in []entry, n uint32) []entry {
	if n > 0 && uint32(len(in)) > n {
		return in[:n]
	}
	return in
}
