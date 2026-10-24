package main

// Promote runs one blue/green update cycle for `root`, with `version` already staged in the standby slot.
// start(slot) and probe() are injected so the decision path is unit-tested with fakes.
// On success the standby is committed active; on failure `active` is flipped back, the bad core stopped, and the caller restarts the good slot.
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
		// Start failed: flip straight back.
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
