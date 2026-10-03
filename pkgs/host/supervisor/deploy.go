package main

import (
	"fmt"
	"os"
	"path/filepath"
)

// Activate atomically points the `active` symlink under `root` at `slot`. The
// flip is what makes a blue/green switch (and its rollback) instant.
func Activate(root string, slot Slot) error {
	link := filepath.Join(root, "active")
	// A symlink cannot be replaced atomically in place; write a temp link and
	// rename it over the old one (rename is atomic on the same filesystem).
	tmp := link + ".tmp"
	_ = os.Remove(tmp)
	if err := os.Symlink(string(slot), tmp); err != nil {
		return fmt.Errorf("staging active symlink: %w", err)
	}
	if err := os.Rename(tmp, link); err != nil {
		return fmt.Errorf("flipping active symlink: %w", err)
	}
	return nil
}

// ActiveSlot reads which slot the `active` symlink under `root` points at.
func ActiveSlot(root string) (Slot, error) {
	target, err := os.Readlink(filepath.Join(root, "active"))
	if err != nil {
		return "", fmt.Errorf("reading active symlink: %w", err)
	}
	return Slot(filepath.Base(target)), nil
}
