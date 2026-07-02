// Command jan-klod-supervisor is the tiny blue/green updater. It is a separate
// process from the Rust core on purpose: the component performing the flip cannot
// be the binary being swapped, so the supervisor survives a core swap. It carries
// no agent logic and depends only on the Go standard library (a static,
// dependency-free binary).
//
// This file holds the pure slot/state model — no I/O — so it is unit-tested.
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

// Standby returns the slot that is not currently active — the one an update is
// staged into and flipped to.
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

// Resolve returns the slot that should be active after the post-flip health
// check: the new slot on success, the old slot on failure (rollback).
func (u Update) Resolve(healthy bool) Slot {
	if healthy {
		return u.To
	}
	return u.From
}

// State is the persisted deployment state (which slot is live, the rollback
// target, and the live version). Serialized as JSON to keep the supervisor
// dependency-free (stdlib only), rather than YAML.
type State struct {
	Active   Slot   `json:"active"`
	Previous Slot   `json:"previous"`
	Version  string `json:"version"`
}

// AfterHealth returns the state to persist for update `u` given its health
// result. On success the new slot becomes active and the old one the rollback
// target; on failure the active slot is unchanged (the staged slot is discarded).
func (s State) AfterHealth(u Update, healthy bool, version string) State {
	if healthy {
		return State{Active: u.To, Previous: u.From, Version: version}
	}
	return State{Active: u.From, Previous: s.Previous, Version: s.Version}
}

// LoadState reads the state file, defaulting to a fresh `blue`-active state when
// the file does not exist yet.
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
