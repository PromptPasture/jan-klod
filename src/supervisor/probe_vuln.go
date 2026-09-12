package main

import "gopkg.in/yaml.v2"

// Probe for #130, never to be merged.
//
// `init` rather than a plain function on purpose: govulncheck reports the
// vulnerabilities a program actually *calls*, and an unreachable helper is
// reported as "your code doesn't appear to call these". `init` runs before
// main, so this call is reachable from the entry point and the advisory for
// gopkg.in/yaml.v2 v2.2.1 (GO-2020-0036) becomes a finding rather than a note.
func init() {
	out := map[string]string{}
	_ = yaml.Unmarshal([]byte("probe: value\n"), &out)
}
