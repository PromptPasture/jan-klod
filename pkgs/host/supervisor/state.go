// Command jan-klod-supervisor is the blue/green updater; it runs as its own process so it survives a core swap.
// It uses only the Go standard library, so it builds as a static binary.
package main

import (
	"encoding/json"
	"fmt"
	"os"
)

// Slot is one of the two blue/green deployment slots.
type Slot string

const (
	// Blue is one deployment slot.
	Blue Slot = "blue"
	// Green is the other deployment slot.
	Green Slot = "green"
)

// Standby returns the slot not currently active: the target of a staged update.
func Standby(active Slot) Slot {
	if active == Blue {
		return Green
	}
	return Blue
}

// Update is a planned flip from the active slot to the standby slot.
type Update struct {
	From Slot
	To   Slot
}

// Plan an update for the given active slot: stage into and flip to standby.
func Plan(active Slot) Update {
	return Update{From: active, To: Standby(active)}
}

// Resolve returns the slot to keep active after the health check: new on success, old (rollback) on failure.
func (u Update) Resolve(healthy bool) Slot {
	if healthy {
		return u.To
	}
	return u.From
}

// State is the persisted deployment state. Stored as JSON, not YAML, to stay stdlib-only.
type State struct {
	Active   Slot   `json:"active"`
	Previous Slot   `json:"previous"`
	Version  string `json:"version"`
}

// AfterHealth returns the state to persist for `u`. On failure the active slot is unchanged and the staged slot discarded.
func (s State) AfterHealth(u Update, healthy bool, version string) State {
	if healthy {
		return State{Active: u.To, Previous: u.From, Version: version}
	}
	return State{Active: u.From, Previous: s.Previous, Version: s.Version}
}

// LoadState reads the state file; a missing file yields a fresh `blue`-active state.
func LoadState(path string) (State, error) {
	data, err := os.ReadFile(path)
	if os.IsNotExist(err) {
		return State{Active: Blue}, nil
	}
	if err != nil {
		return State{}, fmt.Errorf("reading state %s: %w", path, err)
	}
	var state State
	if err := json.Unmarshal(data, &state); err != nil {
		return State{}, fmt.Errorf("parsing state %s: %w", path, err)
	}
	return state, nil
}

// Save writes the state file (pretty JSON).
func (s State) Save(path string) error {
	data, err := json.MarshalIndent(s, "", "  ")
	if err != nil {
		return fmt.Errorf("encoding state: %w", err)
	}
	if err := os.WriteFile(path, data, 0o644); err != nil {
		return fmt.Errorf("writing state %s: %w", path, err)
	}
	return nil
}
