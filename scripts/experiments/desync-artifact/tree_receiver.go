package main

import (
	"encoding/json"
	"errors"
	"io"
	"net/http"
	"os"
	"path/filepath"
	"sort"
	"strings"
)

func durableTreeJSON(path string, value any) error {
	b, err := json.MarshalIndent(value, "", "  ")
	if err != nil {
		return err
	}
	if int64(len(b)) > maxTreeBytes {
		return errors.New("tree metadata admission exceeded")
	}
	return durableJSON(path, value)
}

func catalogIdentity(c TreeCatalog) TreeOffer {
	b, _ := json.Marshal(c)
	return TreeOffer{Protocol: deliveryProtocol, ID: sumBytes(b), CatalogBytes: int64(len(b)),
		Endpoint: "http://127.0.0.1:1", Token: strings.Repeat("0", 64), Session: strings.Repeat("0", 32)}
}

func treeObject(s *slot, path string) (string, error) {
	if !portablePath(path) {
		return "", errors.New("invalid managed path")
	}
	return filepath.Join(s.work, "build", filepath.FromSlash(path)), nil
}

func verifyInstalled(s *slot, current TreeInstalled) ([]*os.File, error) {
	return verifyInstalledMode(s, current, true)
}

func verifyInstalledMode(s *slot, current TreeInstalled, verifyContent bool) ([]*os.File, error) {
	if current.Protocol != deliveryProtocol || current.Root != s.root || current.State != "ready" ||
		!validHex(current.Delivery, 16) || len(current.Files) > maxTreeFiles || len(current.Files) != len(current.Catalog.Entries) {
		return nil, errors.New("invalid installed tree")
	}
	offer := catalogIdentity(current.Catalog)
	if offer.ID != current.Artifact {
		return nil, errors.New("installed catalog identity changed")
	}
	if err := validateCatalog(current.Catalog, offer); err != nil {
		return nil, err
	}
	var held []*os.File
	fail := func(err error) ([]*os.File, error) { closeFiles(held); return nil, err }
	root, err := openDirectory(filepath.Join(s.work, "build"))
	if err != nil {
		return fail(err)
	}
	held = append(held, root)
	id, err := nativeIdentity(root)
	if err != nil || id != current.BuildRoot {
		return fail(errors.New("installed build root changed"))
	}
	dirs := map[string]bool{".": true}
	for i, e := range current.Catalog.Entries {
		owned := current.Files[i]
		if owned.Path != e.Path || owned.Hash != e.Artifact.SHA256 || owned.Identity == "" || owned.Mode != e.Mode {
			return fail(errors.New("installed inventory changed"))
		}
		parts := strings.Split(e.Path, "/")
		for n := 1; n < len(parts); n++ {
			rel := strings.Join(parts[:n], "/")
			if dirs[rel] {
				continue
			}
			p, _ := treeObject(s, rel)
			dir, err := openDirectory(p)
			if err != nil {
				return fail(err)
			}
			held = append(held, dir)
			dirs[rel] = true
		}
		p, _ := treeObject(s, e.Path)
		f, err := openSealed(p, owned.Identity, false)
		if err != nil {
			return fail(err)
		}
		held = append(held, f)
		if !verifyContent {
			continue
		}
		if err := verifyHandle(f, Manifest{TargetBytes: e.Artifact.Bytes, Expected: e.Artifact.SHA256}); err != nil {
			return fail(err)
		}
		info, err := f.Stat()
		if err != nil || uint32(info.Mode().Perm()) != e.Mode {
			return fail(errors.New("installed file metadata changed"))
		}
		attributes, modified, err := fileMetadata(f)
		if err != nil || attributes != e.Attributes || modified != e.Modified {
			return fail(errors.New("installed native metadata changed"))
		}
	}
	actual, err := scanAdoption(s, current.BuildRoot)
	if err != nil {
		return fail(err)
	}
	if len(actual) != len(current.Files) {
		return fail(errors.New("unmanaged files remain in the build; review them before running"))
	}
	expected := make(map[string]string, len(current.Files))
	for _, f := range current.Files {
		expected[f.Path] = f.Identity
	}
	for _, f := range actual {
		if expected[f.Path] != f.Identity {
			return fail(errors.New("unmanaged or replaced build file"))
		}
	}
	return held, nil
}

func verifyTree(s *slot, artifact, delivery string) ([]*os.File, error) {
	if _, err := os.Lstat(filepath.Join(s.work, "tree-intent.json")); !os.IsNotExist(err) {
		return nil, errors.New("tree update is pending")
	}
	var current TreeInstalled
	if err := readTreeControl(filepath.Join(s.work, "tree-installed.json"), &current); err != nil {
		return nil, err
	}
	if current.Artifact != artifact || current.Delivery != delivery {
		return nil, errors.New("requested tree changed")
	}
	return verifyInstalled(s, current)
}

func fetchCatalog(offer TreeOffer, client *http.Client) (TreeCatalog, error) {
	var cat TreeCatalog
	req, _ := http.NewRequest(http.MethodGet, offer.Endpoint+"/tree/v1/catalog", nil)
	req.Header.Set("Authorization", offer.Token)
	setPublicationHeaders(req.Header, offer.ID, offer.Session)
	res, err := client.Do(req)
	if err != nil {
		return cat, errors.New("tree catalog unavailable")
	}
	defer res.Body.Close()
	if res.StatusCode != 200 || res.ContentLength != offer.CatalogBytes || !publicationHeadersMatch(res.Header, offer.ID, offer.Session) {
		return cat, errors.New("tree catalog response refused")
	}
	b, err := io.ReadAll(io.LimitReader(res.Body, offer.CatalogBytes+1))
	if err != nil || int64(len(b)) != offer.CatalogBytes || sumBytes(b) != offer.ID {
		return cat, errors.New("tree catalog digest mismatch")
	}
	decoder := json.NewDecoder(strings.NewReader(string(b)))
	decoder.DisallowUnknownFields()
	if err := decoder.Decode(&cat); err != nil {
		return cat, errors.New("invalid tree catalog")
	}
	if err := validateCatalog(cat, offer); err != nil {
		return cat, err
	}
	return cat, nil
}

func scanAdoption(s *slot, rootID string) ([]OwnedFile, error) {
	root, err := openDirectory(filepath.Join(s.work, "build"))
	if err != nil {
		return nil, err
	}
	defer root.Close()
	id, err := nativeIdentity(root)
	if err != nil || id != rootID {
		return nil, errors.New("adoption root identity changed")
	}
	var result []OwnedFile
	queue := []string{""}
	dirs := 0
	for len(queue) > 0 {
		rel := queue[0]
		queue = queue[1:]
		dirs++
		if dirs > maxTreeFiles {
			return nil, errors.New("adoption admission exceeded")
		}
		parentPath := filepath.Join(s.work, "build", filepath.FromSlash(rel))
		parents, _, err := holdDirectoryChain(parentPath)
		if err != nil {
			closeFiles(parents)
			return nil, err
		}
		dir, err := openDirectory(parentPath)
		if err != nil {
			closeFiles(parents)
			return nil, err
		}
		for {
			batch, err := dir.ReadDir(64)
			if err != nil && err != io.EOF {
				dir.Close()
				closeFiles(parents)
				return nil, err
			}
			for _, entry := range batch {
				path := entry.Name()
				if rel != "" {
					path = rel + "/" + path
				}
				if !portablePath(path) || entry.Type()&os.ModeSymlink != 0 {
					dir.Close()
					closeFiles(parents)
					return nil, errors.New("unsafe adoption path")
				}
				if entry.IsDir() {
					queue = append(queue, path)
					if len(queue)+dirs > maxTreeFiles {
						dir.Close()
						closeFiles(parents)
						return nil, errors.New("adoption admission exceeded")
					}
					continue
				}
				if len(result) >= maxTreeFiles {
					dir.Close()
					closeFiles(parents)
					return nil, errors.New("adoption admission exceeded")
				}
				p, _ := treeObject(s, path)
				f, err := openSealed(p, "", false)
				if err != nil {
					dir.Close()
					closeFiles(parents)
					return nil, err
				}
				identity, err := nativeIdentity(f)
				info, statErr := f.Stat()
				f.Close()
				if err != nil || statErr != nil {
					dir.Close()
					closeFiles(parents)
					return nil, errors.New("invalid adoption file")
				}
				result = append(result, OwnedFile{Path: path, Identity: identity, Mode: uint32(info.Mode().Perm())})
			}
			if err == io.EOF {
				break
			}
		}
		dir.Close()
		closeFiles(parents)
	}
	return result, nil
}

func planTree(s *slot, cfg TreeConfig, cat TreeCatalog) (TreeIntent, error) {
	intent := TreeIntent{Protocol: deliveryProtocol, Root: s.root, Artifact: cfg.Offer.ID, Delivery: cfg.Delivery, Previous: cfg.Previous, Catalog: cat, Old: []OwnedFile{}, Moves: []TreeMove{}, Retired: make(map[string]string)}
	var current TreeInstalled
	err := readTreeControl(filepath.Join(s.work, "tree-installed.json"), &current)
	if err == nil {
		if cfg.Previous != current.Artifact || cfg.Delivery == current.Delivery {
			return intent, errors.New("new tree needs exact previous artifact and a new delivery")
		}
		held, err := verifyInstalledMode(s, current, false)
		if err != nil {
			return intent, err
		}
		defer closeFiles(held)
		intent.Old = current.Files
		intent.BuildRoot = current.BuildRoot
	} else if os.IsNotExist(err) {
		if cfg.Previous != "" {
			return intent, errors.New("previous installed tree is absent")
		}
		if cfg.AdoptRoot != "" {
			intent.Old, err = scanAdoption(s, cfg.AdoptRoot)
			if err != nil {
				return intent, err
			}
		} else {
			if err := os.Mkdir(filepath.Join(s.work, "build"), 0700); err != nil {
				return intent, errors.New("existing build requires explicit native-root adoption")
			}
		}
		root, err := openDirectory(filepath.Join(s.work, "build"))
		if err != nil {
			return intent, err
		}
		intent.BuildRoot, err = nativeIdentity(root)
		root.Close()
		if err != nil {
			return intent, err
		}
	} else {
		return intent, err
	}
	desired := make(map[string]bool)
	oldPaths := make(map[string]bool)
	for _, e := range cat.Entries {
		desired[e.Path] = true
	}
	for _, f := range intent.Old {
		oldPaths[f.Path] = true
	}
	used := make(map[string]bool)
	for _, old := range intent.Old {
		if desired[old.Path] {
			continue
		}
		dir := filepath.Join(s.work, "files", sumBytes([]byte(old.Path)))
		if _, err := os.Lstat(dir); os.IsNotExist(err) {
			continue
		} else if err != nil {
			return intent, err
		}
		control, err := openSlot(dir)
		if err != nil {
			return intent, err
		}
		m, _, err := control.manifest()
		if err != nil || m.TreePath != old.Path || m.Receiver != old.Identity {
			control.Close()
			return intent, errors.New("obsolete file metadata identity changed")
		}
		intent.Retired[old.Path] = control.root
		control.Close()
	}
	for _, e := range cat.Entries {
		if oldPaths[e.Path] {
			continue
		}
		for _, old := range intent.Old {
			if !desired[old.Path] && !used[old.Path] && old.Hash != "" && old.Hash == e.Artifact.SHA256 {
				intent.Moves = append(intent.Moves, TreeMove{old.Path, e.Path, old.Identity})
				used[old.Path] = true
				break
			}
		}
	}
	desiredDirs := make(map[string]bool)
	for _, e := range cat.Entries {
		for parent := filepath.ToSlash(filepath.Dir(e.Path)); parent != "."; parent = filepath.ToSlash(filepath.Dir(parent)) {
			desiredDirs[parent] = true
		}
	}
	oldDirs := make(map[string]bool)
	for _, old := range intent.Old {
		for parent := filepath.ToSlash(filepath.Dir(old.Path)); parent != "."; parent = filepath.ToSlash(filepath.Dir(parent)) {
			if !desiredDirs[parent] {
				oldDirs[parent] = true
			}
		}
	}
	for path := range oldDirs {
		p, _ := treeObject(s, path)
		f, err := openDirectory(p)
		if err != nil {
			return intent, err
		}
		id, err := nativeIdentity(f)
		f.Close()
		if err != nil {
			return intent, err
		}
		intent.EmptyDirs = append(intent.EmptyDirs, OwnedDirectory{path, id})
	}
	sort.Slice(intent.EmptyDirs, func(i, j int) bool { return len(intent.EmptyDirs[i].Path) > len(intent.EmptyDirs[j].Path) })
	preview := TreeInstalled{Protocol: deliveryProtocol, Root: s.root, BuildRoot: intent.BuildRoot, Artifact: intent.Artifact, Delivery: intent.Delivery, State: "ready", Catalog: cat, Files: []OwnedFile{}}
	for _, e := range cat.Entries {
		preview.Files = append(preview.Files, OwnedFile{e.Path, strings.Repeat("0", 25), e.Artifact.SHA256, e.Mode})
	}
	b, err := json.MarshalIndent(preview, "", "  ")
	if err != nil || int64(len(b)) > maxTreeBytes {
		return intent, errors.New("installed metadata admission exceeded")
	}
	return intent, nil
}

func receiveTree(s *slot, cfg TreeConfig, client *http.Client) error {
	var installed TreeInstalled
	if err := readTreeControl(filepath.Join(s.work, "tree-installed.json"), &installed); err == nil && installed.Artifact == cfg.Offer.ID && installed.Delivery == cfg.Delivery {
		if _, err := os.Lstat(filepath.Join(s.work, "tree-intent.json")); os.IsNotExist(err) {
			held, err := verifyTree(s, cfg.Offer.ID, cfg.Delivery)
			closeFiles(held)
			if err == nil {
				return nil
			}
			owned, err := verifyInstalledMode(s, installed, false)
			closeFiles(owned)
			if err != nil {
				return err
			}
			intent := TreeIntent{Protocol: deliveryProtocol, Root: s.root, BuildRoot: installed.BuildRoot, Artifact: cfg.Offer.ID, Delivery: cfg.Delivery, Previous: cfg.Previous, Catalog: installed.Catalog, Old: installed.Files, Retired: make(map[string]string)}
			if err := durableTreeJSON(filepath.Join(s.work, "tree-intent.json"), intent); err != nil {
				return err
			}
		}
	}
	intentPath := filepath.Join(s.work, "tree-intent.json")
	var intent TreeIntent
	if err := readTreeControl(intentPath, &intent); os.IsNotExist(err) {
		cat, err := fetchCatalog(cfg.Offer, client)
		if err != nil {
			return err
		}
		intent, err = planTree(s, cfg, cat)
		if err != nil {
			return err
		}
		if err := durableTreeJSON(intentPath, intent); err != nil {
			return err
		}
		crashCheckpoint(Config{CrashPhase: cfg.CrashPhase}, "tree_intent")
	} else if err != nil {
		return err
	}
	if intent.Protocol != deliveryProtocol || intent.Root != s.root || intent.Artifact != cfg.Offer.ID || intent.Delivery != cfg.Delivery || intent.Previous != cfg.Previous || len(intent.Old) > maxTreeFiles || len(intent.Moves) > maxTreeFiles {
		return errors.New("tree intent identity changed")
	}
	if err := validateCatalog(intent.Catalog, cfg.Offer); err != nil {
		return err
	}
	root, err := openDirectory(filepath.Join(s.work, "build"))
	if err != nil {
		return err
	}
	defer root.Close()
	id, err := nativeIdentity(root)
	if err != nil || id != intent.BuildRoot {
		return errors.New("managed build root changed")
	}
	desired := make(map[string]TreeEntry)
	owned := make(map[string]OwnedFile)
	moved := make(map[string]bool)
	for _, e := range intent.Catalog.Entries {
		desired[e.Path] = e
	}
	for _, f := range intent.Old {
		if !portablePath(f.Path) || f.Identity == "" {
			return errors.New("invalid old inventory")
		}
		owned[f.Path] = f
	}
	moved, err = stageTreeMoves(s, intent, owned, desired, cfg.CrashPhase)
	if err != nil {
		return err
	}
	// No cross-file seed reuse follows this point. Obsolete owned files can be
	// removed before additions, freeing their actual allocation without rollback.
	for _, old := range intent.Old {
		if _, keep := desired[old.Path]; keep {
			continue
		}
		if !moved[old.Path] {
			if err := removeObsolete(s, old); err != nil {
				return err
			}
		}
		if meta := intent.Retired[old.Path]; meta != "" {
			if err := retireControl(s, old.Path, meta); err != nil {
				return err
			}
		}
		crashCheckpoint(Config{CrashPhase: cfg.CrashPhase}, "tree_remove")
	}
	if err := removeEmptyDirectories(s, intent); err != nil {
		return err
	}
	if err := finishTreeMoves(s, intent, owned, cfg.CrashPhase); err != nil {
		return err
	}
	result := TreeInstalled{Protocol: deliveryProtocol, Root: s.root, BuildRoot: intent.BuildRoot, Artifact: intent.Artifact, Delivery: intent.Delivery, State: "ready", Catalog: intent.Catalog, Files: []OwnedFile{}}
	fileParents, err := managedDirectory(s.work, "files")
	defer closeFiles(fileParents)
	if err != nil {
		return err
	}
	for _, e := range intent.Catalog.Entries {
		parents, err := managedDirectory(filepath.Join(s.work, "build"), filepath.Dir(e.Path))
		if err != nil {
			closeFiles(parents)
			return err
		}
		key := sumBytes([]byte(e.Path))
		metadataParents, err := managedDirectory(s.work, "files/"+key)
		if err != nil {
			closeFiles(metadataParents)
			closeFiles(parents)
			return err
		}
		fileSlot, err := openSlot(filepath.Join(s.work, "files", key))
		if err != nil {
			closeFiles(metadataParents)
			closeFiles(parents)
			return err
		}
		delivery := sumBytes([]byte(cfg.Delivery + ":" + e.Path))[:32]
		fileCfg := ReceiveConfig{Offer: entryOffer(cfg.Offer, e), Delivery: delivery, Stash: cfg.Stash, Memory: cfg.Memory, TreePath: e.Path}
		if old, ok := owned[e.Path]; ok {
			fileCfg.AdoptReceiver = old.Identity
			p, _ := treeObject(s, e.Path)
			f, openErr := openOwnedMetadata(p, old.Identity)
			if openErr != nil {
				fileSlot.Close()
				closeFiles(metadataParents)
				closeFiles(parents)
				return openErr
			}
			openErr = f.Chmod(0600)
			f.Close()
			if openErr != nil {
				fileSlot.Close()
				closeFiles(metadataParents)
				closeFiles(parents)
				return openErr
			}
		}
		if m, _, err := fileSlot.manifest(); err == nil && m.DeliveryID != delivery {
			fileCfg.Previous = m.ArtifactID
		}
		m, digest, err := prepareRemoteSlot(fileSlot, fileCfg, client)
		if err == nil {
			index, openErr := openSealed(filepath.Join(fileSlot.work, "target.caibx"), "", false)
			if openErr != nil {
				err = openErr
			} else {
				idx, checkErr := checkedIndex(index, fileCfg.Offer)
				if checkErr != nil {
					err = checkErr
				} else {
					store := newRemoteStore(fileCfg.Offer, client, idx, cfg.Peer)
					err = reconstruct(fileSlot, m, digest, Config{Average: e.Artifact.Average, StashBytes: cfg.Stash}, idx, store, func() error { return nil }, func() map[string]any {
						return withScope(transportScope(fileCfg.Offer.Endpoint, fileCfg.Offer.Peer), map[string]any{"downloaded_payload_bytes": store.bytes.Load()})
					})
				}
				index.Close()
			}
		}
		if err == nil {
			p, _ := treeObject(s, e.Path)
			f, openErr := openSealed(p, m.Receiver, true)
			if openErr != nil {
				err = openErr
			} else {
				err = f.Chmod(os.FileMode(e.Mode))
				if err == nil {
					err = applyFileMetadata(f, e.Attributes, e.Modified)
				}
				if err == nil {
					err = f.Sync()
				}
				f.Close()
			}
		}
		if err == nil {
			result.Files = append(result.Files, OwnedFile{e.Path, m.Receiver, e.Artifact.SHA256, e.Mode})
		}
		fileSlot.Close()
		closeFiles(metadataParents)
		closeFiles(parents)
		if err != nil {
			return err
		}
		crashCheckpoint(Config{CrashPhase: cfg.CrashPhase}, "tree_file")
	}
	held, err := verifyInstalled(s, result)
	if err != nil {
		return err
	}
	defer closeFiles(held)
	if err := durableTreeJSON(filepath.Join(s.work, "tree-installed.json"), result); err != nil {
		return err
	}
	crashCheckpoint(Config{CrashPhase: cfg.CrashPhase}, "tree_ready")
	return os.Remove(intentPath)
}
