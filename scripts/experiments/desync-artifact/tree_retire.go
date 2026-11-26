package main

import (
	"errors"
	"os"
	"path/filepath"
)

func removeObsolete(s *slot, old OwnedFile) error {
	p, err := treeObject(s, old.Path)
	if err != nil {
		return err
	}
	parents, _, err := holdDirectoryChain(filepath.Dir(p))
	defer closeFiles(parents)
	if os.IsNotExist(err) {
		return nil
	}
	if err != nil {
		return err
	}
	f, err := openOwnedMetadata(p, old.Identity)
	if os.IsNotExist(err) {
		return nil
	}
	if err != nil {
		return err
	}
	err = f.Chmod(0600)
	f.Close()
	if err != nil {
		return err
	}
	return removeIdentity(p, old.Identity)
}

// The parent generation intent pins this directory before any payload writes.
// Interrupted cleanup does not need a surviving child receipt as authority.
func retireControl(s *slot, path, expectedRoot string) error {
	if !portablePath(path) {
		return errors.New("invalid retired path")
	}
	dir := filepath.Join(s.work, "files", sumBytes([]byte(path)))
	parents, _, err := holdDirectoryChain(filepath.Dir(dir))
	defer closeFiles(parents)
	if os.IsNotExist(err) {
		return nil
	}
	if err != nil {
		return err
	}
	if _, err := os.Lstat(dir); os.IsNotExist(err) {
		return nil
	} else if err != nil {
		return err
	}
	control, err := openSlot(dir)
	if err != nil {
		return err
	}
	if control.root != expectedRoot {
		control.Close()
		return errors.New("retired metadata directory changed")
	}
	allowed := map[string]bool{"slot.lock": true, "manifest.json": true, "state.json": true, "result.json": true,
		"error.json": true, "target.caibx": true, "next.caibx": true, "generation-intent.json": true,
		"seed.new": true, "reservation.json": true,
		receiverIndexFile: true, receiverIndexMeta: true, receiverIndexFile + ".next": true, receiverIndexMeta + ".next": true}
	listing, err := openDirectory(dir)
	if err != nil {
		control.Close()
		return err
	}
	entries, readErr := listing.ReadDir(32)
	listing.Close()
	if readErr != nil || len(entries) > 24 {
		control.Close()
		return errors.New("unexpected retired metadata entries")
	}
	identities := make(map[string]string)
	for _, entry := range entries {
		name := entry.Name()
		base := name
		if len(base) > 5 && base[len(base)-5:] == ".next" {
			base = base[:len(base)-5]
		}
		if !allowed[base] || entry.IsDir() {
			control.Close()
			return errors.New("unrecognized metadata is preserved for review")
		}
		if name == "slot.lock" {
			identities[name], err = nativeIdentity(control.guard)
		} else {
			f, openErr := openSealed(filepath.Join(dir, name), "", false)
			if openErr != nil {
				control.Close()
				return openErr
			}
			identities[name], err = nativeIdentity(f)
			f.Close()
		}
		if err != nil {
			control.Close()
			return err
		}
	}
	control.Close()
	for name, id := range identities {
		if err := removeIdentity(filepath.Join(dir, name), id); err != nil && !os.IsNotExist(err) {
			return err
		}
	}
	return removeIdentity(dir, expectedRoot)
}
