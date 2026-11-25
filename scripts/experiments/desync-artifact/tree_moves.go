package main

import (
	"errors"
	"os"
	"path/filepath"
)

func matchingObject(path, id string) (bool, error) {
	f, err := openSealed(path, id, false)
	if os.IsNotExist(err) {
		return false, nil
	}
	if err != nil {
		return false, err
	}
	f.Close()
	return true, nil
}

func stageTreeMoves(s *slot, intent TreeIntent, owned map[string]OwnedFile, desired map[string]TreeEntry, crash string) (map[string]bool, error) {
	moved := make(map[string]bool)
	parents, err := managedDirectory(s.work, "relocations")
	defer closeFiles(parents)
	if err != nil {
		return nil, err
	}
	for _, move := range intent.Moves {
		old, ok := owned[move.From]
		entry, wanted := desired[move.To]
		if !ok || old.Identity != move.Identity || old.Hash != entry.Artifact.SHA256 || !wanted || !portablePath(move.To) {
			return nil, errors.New("invalid move intent")
		}
		if _, conflict := desired[move.From]; conflict {
			return nil, errors.New("move would displace a desired path")
		}
		staged := filepath.Join(s.work, "relocations", sumBytes([]byte(move.From))+".bin")
		from, _ := treeObject(s, move.From)
		to, _ := treeObject(s, move.To)
		if done, err := matchingObject(to, move.Identity); err == nil && done {
			moved[move.From] = true
			continue
		}
		if exists, err := matchingObject(staged, move.Identity); err != nil {
			return nil, err
		} else if !exists {
			fromParents, _, err := holdDirectoryChain(filepath.Dir(from))
			if err != nil {
				closeFiles(fromParents)
				return nil, err
			}
			err = moveIdentity(from, staged, move.Identity)
			closeFiles(fromParents)
			if err != nil {
				return nil, err
			}
		}
		moved[move.From] = true
		crashCheckpoint(Config{CrashPhase: crash}, "tree_move_stage")
	}
	return moved, nil
}

func finishTreeMoves(s *slot, intent TreeIntent, owned map[string]OwnedFile, crash string) error {
	for _, move := range intent.Moves {
		old := owned[move.From]
		parents, err := managedDirectory(filepath.Join(s.work, "build"), filepath.Dir(move.To))
		if err != nil {
			closeFiles(parents)
			return err
		}
		to, _ := treeObject(s, move.To)
		if done, err := matchingObject(to, move.Identity); err != nil {
			closeFiles(parents)
			return err
		} else if !done {
			staged := filepath.Join(s.work, "relocations", sumBytes([]byte(move.From))+".bin")
			err = moveIdentity(staged, to, move.Identity)
			if err != nil {
				closeFiles(parents)
				return err
			}
		}
		closeFiles(parents)
		owned[move.To] = OwnedFile{move.To, move.Identity, old.Hash, old.Mode}
		crashCheckpoint(Config{CrashPhase: crash}, "tree_move")
	}
	return nil
}

func removeEmptyDirectories(s *slot, intent TreeIntent) error {
	dirs := intent.EmptyDirs
	if len(dirs) > maxTreeFiles {
		return errors.New("directory cleanup admission exceeded")
	}
	for _, dir := range dirs {
		p, err := treeObject(s, dir.Path)
		if err != nil {
			return err
		}
		parents, _, err := holdDirectoryChain(filepath.Dir(p))
		if os.IsNotExist(err) {
			closeFiles(parents)
			continue
		}
		if err != nil {
			closeFiles(parents)
			return err
		}
		err = removeIdentity(p, dir.Identity)
		closeFiles(parents)
		if err != nil && !os.IsNotExist(err) {
			if !knownNewTreeObject(s, intent, dir.Path, p) {
				return err
			}
		}
	}
	return nil
}

func knownNewTreeObject(s *slot, intent TreeIntent, path, physical string) bool {
	var wanted *TreeEntry
	for i := range intent.Catalog.Entries {
		if intent.Catalog.Entries[i].Path == path {
			wanted = &intent.Catalog.Entries[i]
			break
		}
	}
	if wanted == nil {
		return false
	}
	for _, move := range intent.Moves {
		if move.To == path {
			ok, err := matchingObject(physical, move.Identity)
			return err == nil && ok
		}
	}
	metadata := filepath.Join(s.work, "files", sumBytes([]byte(path)))
	if _, err := os.Lstat(metadata); err != nil {
		return false
	}
	control, err := openSlot(metadata)
	if err != nil {
		return false
	}
	defer control.Close()
	delivery := sumBytes([]byte(intent.Delivery + ":" + path))[:32]
	if m, _, err := control.manifest(); err == nil && m.ArtifactID == artifactID(wanted.Artifact) && m.TreePath == path && m.DeliveryID == delivery {
		ok, err := matchingObject(physical, m.Receiver)
		return err == nil && ok
	}
	var r receiverReservation
	if _, err := readControl(filepath.Join(metadata, "reservation.json"), &r); err == nil && r.Root == control.root && r.Artifact == artifactID(wanted.Artifact) && r.TreePath == path && r.Delivery == delivery {
		ok, err := matchingObject(physical, r.Receiver)
		return err == nil && ok
	}
	return false
}
