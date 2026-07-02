package main

// Promote runs one blue/green update cycle for `root`, given a state file and the
// version being promoted. The new version is assumed already staged into the
// standby slot. `start(slot)` launches the core in that slot and returns a stop
// func; `probe()` reports whether it is healthy. Both are injected so the whole
// flip → start → health → commit/rollback decision is unit-tested with fakes.
//
// On success the standby slot is committed active. On failure the `active` symlink
// is flipped back and the started (bad) core stopped; the caller restarts the good
// slot. Returns the state that was persisted.
func Promote(
	root, stateFile, version string,
	start func(Slot) (func(), error),
	probe func() bool,
) (State, error) {
	state, err := LoadState(stateFile)
	if err != nil {
		return state, err
	}

	update := Plan(state.Active)
	if err := Activate(root, update.To); err != nil {
		return state, err
	}

	stop, err := start(update.To)
	if err != nil {
		// Could not even start the staged slot — flip straight back.
		_ = Activate(root, update.From)
		return state, err
	}

	healthy := probe()
	next := state.AfterHealth(update, healthy, version)
	if !healthy {
		stop()
		_ = Activate(root, update.From)
	}

	if err := next.Save(stateFile); err != nil {
		return next, err
	}
	return next, nil
}
