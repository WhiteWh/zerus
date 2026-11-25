package main

import (
	"errors"
	"os"
	"path/filepath"
)

type treeIndexInventory struct {
	Protocol int               `json:"protocol"`
	Root     string            `json:"root_identity"`
	Files    map[string]string `json:"files"`
}

func previousIndexes(s *slot) (treeIndexInventory, error) {
	var inv treeIndexInventory
	if err := readTreeControl(filepath.Join(s.work, "index-inventory.json"), &inv); os.IsNotExist(err) {
		return treeIndexInventory{deliveryProtocol, s.root, make(map[string]string)}, nil
	} else if err != nil {
		return inv, err
	}
	if inv.Protocol != deliveryProtocol || inv.Root != s.root || len(inv.Files) > maxTreeFiles {
		return inv, errors.New("index inventory identity changed")
	}
	for key, id := range inv.Files {
		if !validHex(key, 32) || id == "" {
			return inv, errors.New("invalid index inventory")
		}
	}
	return inv, nil
}

func replaceTreeIndex(path, priorIdentity string) error {
	if f, err := openSealed(path, "", false); err == nil {
		id, idErr := nativeIdentity(f)
		info, statErr := f.Stat()
		f.Close()
		if idErr != nil || statErr != nil || info.Size() > maxWireIndex {
			return errors.New("unexpected index object")
		}
		if id != priorIdentity {
			// A previous publication can have committed this exact metadata blob
			// before committing its inventory. Content equality recovers it; a
			// different unregistered object is preserved, never overwritten.
			oldHash, err := hashFile(path)
			if err != nil {
				return err
			}
			newHash, err := hashFile(path + ".next")
			if err != nil {
				return err
			}
			if oldHash != newHash {
				return errors.New("unregistered index is preserved for review")
			}
		}
	} else if !os.IsNotExist(err) {
		return err
	}
	return replaceDurable(path+".next", path)
}

func finishIndexes(s *slot, old treeIndexInventory, current map[string]string) error {
	for key, id := range old.Files {
		if _, keep := current[key]; keep {
			continue
		}
		path := filepath.Join(s.work, "indexes", key+".caibx")
		if err := removeIdentity(path, id); err != nil && !os.IsNotExist(err) {
			return err
		}
	}
	return durableTreeJSON(filepath.Join(s.work, "index-inventory.json"), treeIndexInventory{deliveryProtocol, s.root, current})
}
