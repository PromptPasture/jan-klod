package main

import (
	"net/http"
	"time"
)

// Probe reports whether the core answers a `GET` at healthURL with 200 within
// `timeout` — the liveness check run after a flip to confirm the swapped core
// actually booted and can serve.
func Probe(healthURL string, timeout time.Duration) bool {
	client := &http.Client{Timeout: timeout}
	resp, err := client.Get(healthURL)
	if err != nil {
		return false
	}
	defer func() { _ = resp.Body.Close() }()
	return resp.StatusCode == http.StatusOK
}

// ProbeWithRetries polls Probe up to `attempts` times, `interval` apart, giving a
// freshly-restarted core time to come up. Returns true on the first success.
func ProbeWithRetries(healthURL string, attempts int, interval time.Duration) bool {
	for i := 0; i < attempts; i++ {
		if Probe(healthURL, interval) {
			return true
		}
		time.Sleep(interval)
	}
	return false
}
