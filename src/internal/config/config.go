// Package config loads jan-klod.yaml — the single configuration file that
// declares which extensions to load and carries each extension's own settings.
//
// Each extension owns one section under `extensions`. The host reads only the
// `enabled` flag; the remaining keys are opaque to the core and are served back
// to the extension through the host-config interface (see wit/host-config.wit).
//
// Environment variables of the form ${VAR} or $VAR are expanded from the
// process environment before parsing, so secrets such as API keys stay out of
// the file.
package config

import (
	"fmt"
	"os"

	"gopkg.in/yaml.v3"
)

// Config is the parsed jan-klod.yaml.
type Config struct {
	Extensions map[string]ExtensionConfig `yaml:"extensions"`
}

// ExtensionConfig is one extension's section. Enabled gates loading; Settings
// holds every other key, handed to the extension via host-config.
type ExtensionConfig struct {
	Enabled  bool
	Settings map[string]any
}

// UnmarshalYAML accepts either a bool shorthand (`store-memory: true`) or a map
// with an optional `enabled` flag (defaulting to true when the section exists).
func (e *ExtensionConfig) UnmarshalYAML(node *yaml.Node) error {
	if node.Kind == yaml.ScalarNode {
		var enabled bool
		if err := node.Decode(&enabled); err != nil {
			return fmt.Errorf("extension shorthand must be a bool: %w", err)
		}
		e.Enabled = enabled
		e.Settings = map[string]any{}
		return nil
	}

	var raw map[string]any
	if err := node.Decode(&raw); err != nil {
		return fmt.Errorf("extension section must be a map or bool: %w", err)
	}

	e.Enabled = true
	if v, ok := raw["enabled"]; ok {
		enabled, isBool := v.(bool)
		if !isBool {
			return fmt.Errorf("enabled must be a bool, got %T", v)
		}
		e.Enabled = enabled
		delete(raw, "enabled")
	}
	e.Settings = raw
	return nil
}

// Load reads, expands environment variables in, and parses the config at path.
func Load(path string) (*Config, error) {
	raw, err := os.ReadFile(path)
	if err != nil {
		return nil, fmt.Errorf("read config %s: %w", path, err)
	}

	expanded := os.Expand(string(raw), os.Getenv)

	var cfg Config
	if err := yaml.Unmarshal([]byte(expanded), &cfg); err != nil {
		return nil, fmt.Errorf("parse config %s: %w", path, err)
	}
	if cfg.Extensions == nil {
		cfg.Extensions = map[string]ExtensionConfig{}
	}
	return &cfg, nil
}
