package main

import (
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"time"
)

// defaultBind is where a promoted core serves and is health-probed.
const defaultBind = "127.0.0.1:8787"

func main() {
	if len(os.Args) < 2 {
		usage()
		os.Exit(2)
	}
	root := rootDir()
	switch os.Args[1] {
	case "status":
		os.Exit(status(root))
	case "promote":
		os.Exit(promote(root))
	default:
		usage()
		os.Exit(2)
	}
}

func usage() {
	fmt.Fprintln(os.Stderr, "usage: jan-klod-supervisor <status|promote>")
	fmt.Fprintln(os.Stderr, "  root dir from JAN_KLOD_HOME (default: ~/.jan-klod)")
}

// rootDir is the blue/green root: JAN_KLOD_HOME, else ~/.jan-klod.
func rootDir() string {
	if home := os.Getenv("JAN_KLOD_HOME"); home != "" {
		return home
	}
	base, err := os.UserHomeDir()
	if err != nil {
		return ".jan-klod"
	}
	return filepath.Join(base, ".jan-klod")
}

func status(root string) int {
	state, err := LoadState(filepath.Join(root, "state.json"))
	if err != nil {
		fmt.Fprintln(os.Stderr, "supervisor:", err)
		return 1
	}
	fmt.Printf("active=%s previous=%s version=%s\n", state.Active, state.Previous, state.Version)
	if slot, err := ActiveSlot(root); err == nil {
		fmt.Printf("active symlink -> %s\n", slot)
	}
	return 0
}

func promote(root string) int {
	stateFile := filepath.Join(root, "state.json")

	// Launch the core in `slot` serving on the default bind.
	start := func(slot Slot) (func(), error) {
		bin := filepath.Join(root, string(slot), "jan-klod")
		config := filepath.Join(root, "active", "config.yaml")
		ext := filepath.Join(root, "active", "ext")
		cmd := exec.Command(bin, "serve", config, ext, defaultBind)
		cmd.Stdout = os.Stdout
		cmd.Stderr = os.Stderr
		if err := cmd.Start(); err != nil {
			return nil, fmt.Errorf("starting core in %s: %w", slot, err)
		}
		return func() {
			_ = cmd.Process.Kill()
			_, _ = cmd.Process.Wait()
		}, nil
	}
	probe := func() bool {
		return ProbeWithRetries("http://"+defaultBind+"/health", 20, 500*time.Millisecond)
	}

	state, err := Promote(root, stateFile, "", start, probe)
	if err != nil {
		fmt.Fprintln(os.Stderr, "supervisor: promote failed:", err)
		return 1
	}
	fmt.Printf("promote: active slot is now %s\n", state.Active)
	return 0
}
