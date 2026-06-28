package config

import (
	"os"
	"path/filepath"
	"testing"
)

func writeConfig(t *testing.T, body string) string {
	t.Helper()
	path := filepath.Join(t.TempDir(), "jan-klod.yaml")
	if err := os.WriteFile(path, []byte(body), 0o600); err != nil {
		t.Fatalf("write config: %v", err)
	}
	return path
}

func TestLoadBoolShorthandAndMap(t *testing.T) {
	t.Setenv("TEST_API_KEY", "secret-123")

	path := writeConfig(t, `
extensions:
  store-memory: true
  provider-disabled: false
  provider-openai:
    enabled: true
    base-url: https://api.openai.com/v1
    api-key: ${TEST_API_KEY}
    model: gpt-4o-mini
`)

	cfg, err := Load(path)
	if err != nil {
		t.Fatalf("Load: %v", err)
	}

	if got := cfg.Extensions["store-memory"]; !got.Enabled {
		t.Errorf("store-memory: want enabled, got disabled")
	}
	if got := cfg.Extensions["provider-disabled"]; got.Enabled {
		t.Errorf("provider-disabled: want disabled, got enabled")
	}

	openai := cfg.Extensions["provider-openai"]
	if !openai.Enabled {
		t.Errorf("provider-openai: want enabled, got disabled")
	}
	if got := openai.Settings["api-key"]; got != "secret-123" {
		t.Errorf("api-key: want expanded env value, got %v", got)
	}
	if _, ok := openai.Settings["enabled"]; ok {
		t.Errorf("enabled flag should not leak into settings")
	}
	if got := openai.Settings["model"]; got != "gpt-4o-mini" {
		t.Errorf("model: want gpt-4o-mini, got %v", got)
	}
}

func TestLoadMapDefaultsEnabled(t *testing.T) {
	path := writeConfig(t, `
extensions:
  provider-openai:
    model: gpt-4o-mini
`)

	cfg, err := Load(path)
	if err != nil {
		t.Fatalf("Load: %v", err)
	}
	if !cfg.Extensions["provider-openai"].Enabled {
		t.Errorf("map section without enabled: want enabled by default")
	}
}

func TestLoadMissingFile(t *testing.T) {
	if _, err := Load(filepath.Join(t.TempDir(), "nope.yaml")); err == nil {
		t.Errorf("Load missing file: want error, got nil")
	}
}
