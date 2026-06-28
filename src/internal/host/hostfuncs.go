package host

import (
	"bytes"
	"context"
	"encoding/base64"
	"encoding/json"
	"io"
	"net/http"
	"time"

	"github.com/PromptPasture/jan-klod/internal/abi"
	"github.com/tetratelabs/wazero/api"
)

// defaultHTTPTimeout matches wit/host-http.wit (0 = use host default).
const defaultHTTPTimeout = 30 * time.Second

// --- host-config (wit/host-config.wit) ---

type configResult struct {
	Ok    bool            `json:"ok"`
	Value json.RawMessage `json:"value,omitempty"`
	Error string          `json:"error,omitempty"`
}

// configGet serves a value from the calling extension's own config section.
// The value is returned JSON-encoded, as the contract specifies.
func (h *Host) configGet(extName, key string) configResult {
	section, ok := h.configs[extName]
	if !ok {
		return configResult{Error: "backend"}
	}
	v, ok := section[key]
	if !ok {
		return configResult{Error: "key-not-found"}
	}
	raw, err := json.Marshal(v)
	if err != nil {
		return configResult{Error: "backend"}
	}
	return configResult{Ok: true, Value: raw}
}

// --- host-http (wit/host-http.wit) ---

type httpHeader struct {
	Name  string `json:"name"`
	Value string `json:"value"`
}

type httpRequestMsg struct {
	Method    string       `json:"method"`
	URL       string       `json:"url"`
	Headers   []httpHeader `json:"headers,omitempty"`
	Body      string       `json:"body,omitempty"` // base64
	TimeoutMs uint32       `json:"timeout-ms,omitempty"`
}

type httpResultMsg struct {
	Ok      bool         `json:"ok"`
	Status  int          `json:"status,omitempty"`
	Headers []httpHeader `json:"headers,omitempty"`
	Body    string       `json:"body,omitempty"` // base64
	Error   string       `json:"error,omitempty"`
}

// httpFetch performs a synchronous outbound HTTP request on behalf of an
// extension. A completed exchange (including 4xx/5xx) returns ok=true with the
// status and body so the caller can read API error payloads; only transport
// failures return ok=false. This is the ABI's concrete encoding of the WIT
// http-error contract.
func (h *Host) httpFetch(ctx context.Context, reqBytes []byte) httpResultMsg {
	var msg httpRequestMsg
	if err := json.Unmarshal(reqBytes, &msg); err != nil {
		return httpResultMsg{Error: "backend"}
	}

	var body []byte
	if msg.Body != "" {
		decoded, err := base64.StdEncoding.DecodeString(msg.Body)
		if err != nil {
			return httpResultMsg{Error: "backend"}
		}
		body = decoded
	}

	timeout := defaultHTTPTimeout
	if msg.TimeoutMs > 0 {
		timeout = time.Duration(msg.TimeoutMs) * time.Millisecond
	}
	reqCtx, cancel := context.WithTimeout(ctx, timeout)
	defer cancel()

	method := msg.Method
	if method == "" {
		method = http.MethodGet
	}
	req, err := http.NewRequestWithContext(reqCtx, method, msg.URL, bytes.NewReader(body))
	if err != nil {
		return httpResultMsg{Error: "invalid-url"}
	}
	for _, hd := range msg.Headers {
		req.Header.Add(hd.Name, hd.Value)
	}

	resp, err := h.httpClient.Do(req)
	if err != nil {
		if reqCtx.Err() == context.DeadlineExceeded {
			return httpResultMsg{Error: "timeout"}
		}
		return httpResultMsg{Error: "connection-failed"}
	}
	defer func() { _ = resp.Body.Close() }()

	respBody, err := io.ReadAll(resp.Body)
	if err != nil {
		return httpResultMsg{Error: "connection-failed"}
	}

	out := httpResultMsg{Ok: true, Status: resp.StatusCode, Body: base64.StdEncoding.EncodeToString(respBody)}
	for name, values := range resp.Header {
		for _, v := range values {
			out.Headers = append(out.Headers, httpHeader{Name: name, Value: v})
		}
	}
	return out
}

// --- ABI return helpers ---

// readGuestString reads a UTF-8 string from guest memory.
func readGuestString(m api.Module, ptr, size uint32) string {
	if size == 0 {
		return ""
	}
	if b, ok := m.Memory().Read(ptr, size); ok {
		return string(b)
	}
	return ""
}

// returnJSON marshals v, asks the guest to allocate a buffer for it, writes the
// bytes into guest memory, and returns Pack(ptr, len). Returns 0 on any failure
// so the guest can treat a zero result as an error.
func (h *Host) returnJSON(ctx context.Context, m api.Module, v any) uint64 {
	data, err := json.Marshal(v)
	if err != nil {
		return 0
	}

	alloc := m.ExportedFunction("alloc")
	if alloc == nil {
		return 0
	}
	res, err := alloc.Call(ctx, uint64(len(data)))
	if err != nil || len(res) == 0 {
		return 0
	}
	ptr := uint32(res[0])

	if !m.Memory().Write(ptr, data) {
		return 0
	}
	return abi.Pack(ptr, uint32(len(data)))
}
