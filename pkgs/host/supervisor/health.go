package main

import (
	"net/http"
	"time"
)

// Probe reports whether `GET healthURL` returns 200 within `timeout`: the post-flip liveness check.
func Probe(healthURL string, timeout time.Duration) bool {
	client := &http.Client{Timeout: timeout}
	resp, err := client.Get(healthURL)
	if err != nil {
		return false
	}
	defer func() { _ = resp.Body.Close() }()
	return resp.StatusCode == http.StatusOK
}

// ProbeWithRetries polls Probe up to `attempts` times, `interval` apart, while a core is still starting.
func ProbeWithRetries(healthURL string, attempts int, interval time.Duration) bool {
	for i := 0; i < attempts; i++ {
		if Probe(healthURL, interval) {
			return true
		}
		time.Sleep(interval)
	}
	return false
}
