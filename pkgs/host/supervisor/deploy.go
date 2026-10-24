package main

import (
	"fmt"
	"os"
	"path/filepath"
)

// Atomic repoint of the `active` symlink under `root` to `slot`; makes switch and rollback instant.
func Activate(root string, slot Slot) error {
	link := filepath.Join(root, "active")
	// A symlink can't be swapped in place: rename a temp link over the old one (atomic on one fs).
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
