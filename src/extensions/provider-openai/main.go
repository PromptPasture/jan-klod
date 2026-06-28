//go:build wasip1

// provider-openai is a WASM extension implementing the llm-provider contract
// (see wit/llm-provider.wit) against any OpenAI-compatible Chat Completions API
// (OpenAI, Ollama, vLLM, LM Studio, …). This slice supports non-streaming
// completions; streaming is added later.
//
// Configuration (read from its own section via host-config at start):
//
//	base-url   e.g. https://api.openai.com/v1
//	api-key    bearer token (may be empty for local servers)
//	model      default model when a request omits one
//
// ABI (core WASM module, JSON over linear memory):
//
//	alloc(size u32) -> ptr u32
//	free(ptr u32)
//	invoke(ptr u32, len u32) -> u64   // (resultPtr<<32 | resultLen)
//
// Imported from host module "jan-klod": log, config_get, http_fetch.
package main

import (
	"encoding/base64"
	"encoding/json"
	"strings"
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

func configString(key string) string {
	out := callHost(hostConfigGet, []byte(key))
	if out == nil {
		return ""
	}
	var env struct {
		Ok    bool            `json:"ok"`
		Value json.RawMessage `json:"value"`
	}
	if json.Unmarshal(out, &env) != nil || !env.Ok {
		return ""
	}
	var s string
	_ = json.Unmarshal(env.Value, &s)
	return s
}

// --- provider config (populated at start) ---

var (
	cfgBaseURL string
	cfgAPIKey  string
	cfgModel   string
)

// --- ABI request / response ---

type message struct {
	Role    string `json:"role"`
	Content string `json:"content"`
}

type request struct {
	Op          string    `json:"op"`
	Model       string    `json:"model"`
	Messages    []message `json:"messages"`
	MaxTokens   *uint32   `json:"max-tokens"`
	Temperature *float64  `json:"temperature"`
}

type response struct {
	Ok              bool     `json:"ok"`
	Error           string   `json:"error,omitempty"`
	Status          string   `json:"status,omitempty"`
	Content         string   `json:"content,omitempty"`
	FinishReason    string   `json:"finish-reason,omitempty"`
	ID              string   `json:"id,omitempty"`
	SupportedModels []string `json:"supported-models,omitempty"`
}

func dispatch(in []byte) []byte {
	var req request
	if err := json.Unmarshal(in, &req); err != nil {
		return reply(response{Error: "transient"})
	}
	logInfo("provider-openai op=" + req.Op)

	switch req.Op {
	case "lifecycle.init":
		return reply(response{Ok: true})
	case "lifecycle.start":
		cfgBaseURL = strings.TrimRight(configString("base-url"), "/")
		cfgAPIKey = configString("api-key")
		cfgModel = configString("model")
		if cfgBaseURL == "" {
			return reply(response{Error: "base-url not configured"})
		}
		return reply(response{Ok: true})
	case "lifecycle.stop":
		return reply(response{Ok: true})
	case "lifecycle.health":
		return reply(response{Ok: true, Status: "up"})
	case "info":
		return reply(opInfo())
	case "complete":
		return reply(opComplete(req))
	default:
		return reply(response{Error: "transient"})
	}
}

func reply(r response) []byte {
	b, err := json.Marshal(r)
	if err != nil {
		return []byte(`{"ok":false,"error":"transient"}`)
	}
	return b
}

func opInfo() response {
	models := []string{}
	if cfgModel != "" {
		models = append(models, cfgModel)
	}
	return response{Ok: true, ID: "provider-openai", SupportedModels: models}
}

func opComplete(req request) response {
	model := req.Model
	if model == "" {
		model = cfgModel
	}
	if model == "" {
		return response{Error: "model-not-found"}
	}
	if cfgBaseURL == "" {
		return response{Error: "transient"}
	}

	oaReq := map[string]any{
		"model":    model,
		"messages": req.Messages,
		"stream":   false,
	}
	if req.MaxTokens != nil {
		oaReq["max_tokens"] = *req.MaxTokens
	}
	if req.Temperature != nil {
		oaReq["temperature"] = *req.Temperature
	}
	bodyBytes, _ := json.Marshal(oaReq)

	headers := []map[string]string{
		{"name": "Content-Type", "value": "application/json"},
	}
	if cfgAPIKey != "" {
		headers = append(headers, map[string]string{"name": "Authorization", "value": "Bearer " + cfgAPIKey})
	}
	hostReq := map[string]any{
		"method":  "POST",
		"url":     cfgBaseURL + "/chat/completions",
		"headers": headers,
		"body":    base64.StdEncoding.EncodeToString(bodyBytes),
	}
	reqBytes, _ := json.Marshal(hostReq)

	out := callHost(hostHTTPFetch, reqBytes)
	if out == nil {
		return response{Error: "transient"}
	}

	var hostResp struct {
		Ok     bool   `json:"ok"`
		Status int    `json:"status"`
		Body   string `json:"body"`
		Error  string `json:"error"`
	}
	if err := json.Unmarshal(out, &hostResp); err != nil {
		return response{Error: "transient"}
	}
	if !hostResp.Ok {
		return response{Error: "transient"} // transport failure
	}
	if hostResp.Status != 200 {
		return response{Error: mapStatus(hostResp.Status)}
	}

	decoded, err := base64.StdEncoding.DecodeString(hostResp.Body)
	if err != nil {
		return response{Error: "transient"}
	}

	var oaResp struct {
		Choices []struct {
			Message struct {
				Content string `json:"content"`
			} `json:"message"`
			FinishReason string `json:"finish_reason"`
		} `json:"choices"`
	}
	if err := json.Unmarshal(decoded, &oaResp); err != nil || len(oaResp.Choices) == 0 {
		return response{Error: "transient"}
	}
	c := oaResp.Choices[0]
	return response{Ok: true, Content: c.Message.Content, FinishReason: c.FinishReason}
}

// mapStatus maps an HTTP status to a provider-error name (wit/llm-provider.wit).
func mapStatus(status int) string {
	switch {
	case status == 401 || status == 403:
		return "auth-failed"
	case status == 404:
		return "model-not-found"
	case status == 429:
		return "rate-limited"
	default:
		return "transient"
	}
}
