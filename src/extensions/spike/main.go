// Slice 1a gate guest: a TinyGo component that exports the `spike` world's
// `complete` function. It echoes the prompt so the Rust host can prove the
// Component Model round-trip works. Throwaway — delete with the gate.
package main

import spike "jan-klod/spike/jan-klod/spike/spike"

func init() {
	spike.Exports.Complete = func(prompt string) string {
		return "echo: " + prompt
	}
}

// main is required by TinyGo's wasip2 target even though this is a reactor
// component (the export above is the real entry point).
func main() {}
