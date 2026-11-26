// Whole-tree publication retains one payload tree and bounded per-file metadata.
package main

import (
	"crypto/subtle"
	"encoding/json"
	"errors"
	"io"
	"net/http"
	"os"
	"path/filepath"
	"runtime/debug"
	"sort"
	"strconv"
	"strings"
	"sync"
	"time"

	d "github.com/folbricht/desync"
)

const maxTreeFiles = 4096
const maxTreeBytes = 4 * MiB

type TreeEntry struct {
	Path       string   `json:"path"`
	Artifact   Artifact `json:"artifact"`
	Chunks     int      `json:"chunks"`
	IndexBytes int64    `json:"index_bytes"`
	Mode       uint32   `json:"mode"`
	Attributes uint32   `json:"attributes"`
	Modified   int64    `json:"modified"`
}
type TreeCatalog struct {
	Protocol int         `json:"protocol"`
	Backend  string      `json:"backend"`
	Entries  []TreeEntry `json:"entries"`
}
type TreeOffer struct {
	Protocol     int    `json:"protocol"`
	ID           string `json:"artifact_id"`
	CatalogBytes int64  `json:"catalog_bytes"`
	Endpoint     string `json:"endpoint"`
	Token        string `json:"token"`
	Session      string `json:"session"`
	Peer         string `json:"peer_fingerprint,omitempty"`
}
type TreeConfig struct {
	SourceRoot       string     `json:"source_root,omitempty"`
	Average          uint64     `json:"average,omitempty"`
	ServeSeconds     int        `json:"serve_seconds,omitempty"`
	Offer            TreeOffer  `json:"offer"`
	Delivery         string     `json:"delivery_id"`
	Previous         string     `json:"previous_artifact_id,omitempty"`
	AdoptRoot        string     `json:"adopt_root_identity,omitempty"`
	Stash            int64      `json:"stash_bytes"`
	Memory           int64      `json:"memory_bytes"`
	CrashPhase       string     `json:"crash_phase,omitempty"`
	Peer             PeerConfig `json:"peer,omitempty"`
	ExecutorIdentity string     `json:"executor_identity,omitempty"`
}
type OwnedFile struct {
	Path     string `json:"path"`
	Identity string `json:"identity"`
	Hash     string `json:"sha256"`
	Mode     uint32 `json:"mode"`
}
type TreeInstalled struct {
	Protocol  int         `json:"protocol"`
	Root      string      `json:"root_identity"`
	BuildRoot string      `json:"build_root_identity"`
	Artifact  string      `json:"artifact_id"`
	Delivery  string      `json:"delivery_id"`
	State     string      `json:"state"`
	Files     []OwnedFile `json:"files"`
	Catalog   TreeCatalog `json:"catalog"`
}
type TreeMove struct {
	From     string `json:"from"`
	To       string `json:"to"`
	Identity string `json:"identity"`
}
type OwnedDirectory struct {
	Path     string `json:"path"`
	Identity string `json:"identity"`
}
type TreeIntent struct {
	Protocol  int               `json:"protocol"`
	Root      string            `json:"root_identity"`
	BuildRoot string            `json:"build_root_identity"`
	Artifact  string            `json:"artifact_id"`
	Delivery  string            `json:"delivery_id"`
	Previous  string            `json:"previous_artifact_id"`
	Catalog   TreeCatalog       `json:"catalog"`
	Old       []OwnedFile       `json:"old_files"`
	Moves     []TreeMove        `json:"moves"`
	Retired   map[string]string `json:"retired_metadata"`
	EmptyDirs []OwnedDirectory  `json:"empty_directories"`
}

func readTreeControl(path string, value any) error {
	f, err := openSealed(path, "", false)
	if err != nil {
		return err
	}
	defer f.Close()
	b, err := io.ReadAll(io.LimitReader(f, maxTreeBytes+1))
	if err != nil || len(b) > int(maxTreeBytes) {
		return errors.New("tree metadata exceeds admission budget")
	}
	decoder := json.NewDecoder(strings.NewReader(string(b)))
	decoder.DisallowUnknownFields()
	if err := decoder.Decode(value); err != nil {
		return errors.New("invalid tree metadata")
	}
	if decoder.Decode(new(any)) != io.EOF {
		return errors.New("trailing tree metadata")
	}
	return nil
}

func entryOffer(root TreeOffer, e TreeEntry) Offer {
	return Offer{Protocol: wireProtocol, Artifact: e.Artifact, ID: artifactID(e.Artifact), Chunks: e.Chunks,
		IndexLen: e.IndexBytes, Endpoint: root.Endpoint, Token: root.Token, Session: root.Session, Resource: sumBytes([]byte(e.Path)), Peer: root.Peer}
}

func validateCatalog(c TreeCatalog, offer TreeOffer) error {
	b, err := json.Marshal(c)
	if err != nil || int64(len(b)) > maxTreeBytes || sumBytes(b) != offer.ID || int64(len(b)) != offer.CatalogBytes ||
		c.Protocol != deliveryProtocol || c.Backend != deliveryBackend || len(c.Entries) > maxTreeFiles {
		return errors.New("tree catalog identity or size mismatch")
	}
	seen := make(map[string]bool)
	last := ""
	for _, e := range c.Entries {
		if !portablePath(e.Path) || e.Path <= last || seen[strings.ToLower(e.Path)] || e.Mode&^0777 != 0 || e.Attributes&^uint32(0x27) != 0 || e.Modified <= 0 {
			return errors.New("unsafe or colliding tree path")
		}
		for parent := filepath.ToSlash(filepath.Dir(e.Path)); parent != "."; parent = filepath.ToSlash(filepath.Dir(parent)) {
			if seen[strings.ToLower(parent)] {
				return errors.New("file and directory path collision")
			}
		}
		if err := validOffer(entryOffer(offer, e)); err != nil {
			return err
		}
		seen[strings.ToLower(e.Path)] = true
		last = e.Path
	}
	return nil
}

func validTreeOffer(o TreeOffer) error {
	if o.Protocol != deliveryProtocol || !validHex(o.ID, 32) || !validHex(o.Token, 32) || !validHex(o.Session, 16) || o.CatalogBytes < 1 || o.CatalogBytes > maxTreeBytes {
		return errors.New("unsupported tree descriptor")
	}
	return validateEndpoint(o.Endpoint, o.Peer)
}

type treePublisher struct {
	offer      TreeOffer
	catalog    []byte
	entries    map[string]TreeEntry
	sources    map[string]*os.File
	indexRoot  string
	mu         sync.Mutex
	closing    bool
	active     sync.WaitGroup
	cacheKey   string
	cacheIndex *os.File
	cache      *publicationHandler
}

func (p *treePublisher) ServeHTTP(w http.ResponseWriter, r *http.Request) {
	if r.Method != http.MethodGet || r.URL.RawQuery != "" || subtle.ConstantTimeCompare([]byte(r.Header.Get("Authorization")), []byte(p.offer.Token)) != 1 {
		http.Error(w, "refused", http.StatusForbidden)
		return
	}
	p.mu.Lock()
	defer p.mu.Unlock()
	if p.closing {
		http.Error(w, "closed", http.StatusServiceUnavailable)
		return
	}
	// Serial file serving keeps only one chunk lookup live in this publisher.
	p.active.Add(1)
	defer p.active.Done()
	if r.URL.Path == "/tree/v1/catalog" {
		if !publicationHeadersMatch(r.Header, p.offer.ID, p.offer.Session) {
			http.Error(w, "refused", 403)
			return
		}
		setPublicationHeaders(w.Header(), p.offer.ID, p.offer.Session)
		w.Header().Set("Content-Length", strconv.Itoa(len(p.catalog)))
		_, _ = w.Write(p.catalog)
		return
	}
	const prefix = "/tree/v1/files/"
	if !strings.HasPrefix(r.URL.Path, prefix) {
		http.Error(w, "unknown resource", 404)
		return
	}
	parts := strings.SplitN(strings.TrimPrefix(r.URL.Path, prefix), "/", 2)
	if len(parts) != 2 || !validHex(parts[0], 32) {
		http.Error(w, "invalid resource", 400)
		return
	}
	key := parts[0]
	e, ok := p.entries[key]
	if !ok {
		http.Error(w, "unknown resource", 404)
		return
	}
	fileOffer := entryOffer(p.offer, e)
	if !publicationHeadersMatch(r.Header, fileOffer.ID, fileOffer.Session) {
		http.Error(w, "refused", 403)
		return
	}
	if p.cacheKey != key {
		if p.cacheIndex != nil {
			p.cacheIndex.Close()
			p.cacheIndex = nil
			p.cache = nil
			p.cacheKey = ""
		}
		index, err := openSealed(filepath.Join(p.indexRoot, key+".caibx"), "", false)
		if err != nil {
			http.Error(w, "index unavailable", 409)
			return
		}
		idx, err := checkedIndex(index, fileOffer)
		if err != nil {
			index.Close()
			http.Error(w, "index unavailable", 409)
			return
		}
		source := p.sources[key]
		info, err := source.Stat()
		if err != nil {
			index.Close()
			http.Error(w, "source unavailable", 409)
			return
		}
		v, err := makeView(source, Config{Length: info.Size()}, "same")
		if err != nil {
			index.Close()
			http.Error(w, "source unavailable", 409)
			return
		}
		pub := &publisher{view: v, chunks: make(map[d.ChunkID]d.IndexChunk, len(idx.Chunks)), sourceInfo: info, token: p.offer.Token}
		for _, c := range idx.Chunks {
			pub.chunks[c.ID] = c
		}
		p.cache = &publicationHandler{offer: fileOffer, chunks: pub, index: index, admitted: make(chan struct{}, 1)}
		p.cacheIndex = index
		p.cacheKey = key
	}
	clone := r.Clone(r.Context())
	u := *r.URL
	u.Path = "/" + parts[1]
	clone.URL = &u
	p.cache.ServeHTTP(w, clone)
}

func publishTree(configPath string, cfg TreeConfig) error {
	return publishTreeUntil(configPath, cfg, nil, nil)
}

func publishTreeUntil(configPath string, cfg TreeConfig, ready func(TreeOffer) error, release <-chan struct{}) error {
	if cfg.Average < 4096 || cfg.Average > 4*uint64(MiB) || cfg.ServeSeconds < 1 || cfg.ServeSeconds > 604800 {
		return errors.New("invalid tree publication bounds")
	}
	s, err := openSlot(filepath.Dir(configPath))
	if err != nil {
		return err
	}
	defer s.Close()
	parents, _, err := holdDirectoryChain(cfg.SourceRoot)
	defer func() { closeFiles(parents) }()
	if err != nil {
		return err
	}
	indexParents, err := managedDirectory(s.work, "indexes")
	defer func() { closeFiles(indexParents) }()
	if err != nil {
		return err
	}
	oldIndexes, err := previousIndexes(s)
	if err != nil {
		return err
	}
	newIndexes := make(map[string]string)
	cat := TreeCatalog{Protocol: deliveryProtocol, Backend: deliveryBackend, Entries: []TreeEntry{}}
	pub := &treePublisher{entries: make(map[string]TreeEntry), sources: make(map[string]*os.File), indexRoot: filepath.Join(s.work, "indexes")}
	defer func() {
		if pub.cacheIndex != nil {
			pub.cacheIndex.Close()
		}
		for _, f := range pub.sources {
			f.Close()
		}
	}()
	queue := []string{""}
	dirs := 0
	for len(queue) > 0 {
		rel := queue[0]
		queue = queue[1:]
		dirs++
		if dirs > maxTreeFiles || strings.Count(rel, "/") > 32 {
			return errors.New("tree directory admission exceeded")
		}
		dir, err := openDirectory(filepath.Join(cfg.SourceRoot, filepath.FromSlash(rel)))
		if err != nil {
			return err
		}
		parents = append(parents, dir)
		for {
			batch, err := dir.ReadDir(64)
			if err != nil && err != io.EOF {
				return err
			}
			for _, entry := range batch {
				name := entry.Name()
				path := name
				if rel != "" {
					path = rel + "/" + name
				}
				if !portablePath(path) || entry.Type()&os.ModeSymlink != 0 {
					return errors.New("unsupported source tree path")
				}
				if entry.IsDir() {
					queue = append(queue, path)
					if len(queue)+dirs > maxTreeFiles {
						return errors.New("tree directory admission exceeded")
					}
					continue
				}
				if len(cat.Entries) >= maxTreeFiles {
					return errors.New("tree file admission exceeded")
				}
				f, err := openSealed(filepath.Join(cfg.SourceRoot, filepath.FromSlash(path)), "", false)
				if err != nil {
					return err
				}
				key := sumBytes([]byte(path))
				pub.sources[key] = f
				info, err := f.Stat()
				if err != nil {
					return err
				}
				v, err := makeView(f, Config{Length: info.Size()}, "same")
				if err != nil {
					return err
				}
				idx, hash, err := makeIndex(v, cfg.Average)
				if err != nil {
					return err
				}
				if len(idx.Chunks) > maxWireChunks {
					return errors.New("file index exceeds admission budget")
				}
				indexPath := filepath.Join(pub.indexRoot, key+".caibx")
				out, err := openReceiptNext(indexPath + ".next")
				if err != nil {
					return err
				}
				_, writeErr := idx.WriteTo(out)
				syncErr, closeErr := out.Sync(), out.Close()
				if writeErr != nil || syncErr != nil || closeErr != nil {
					return errors.New("tree index publication failed")
				}
				if err := replaceTreeIndex(indexPath, oldIndexes.Files[key]); err != nil {
					return err
				}
				indexHash, err := hashFile(indexPath)
				if err != nil {
					return err
				}
				indexFile, err := openSealed(indexPath, "", false)
				if err != nil {
					return err
				}
				newIndexes[key], err = nativeIdentity(indexFile)
				indexFile.Close()
				if err != nil {
					return err
				}
				a := Artifact{deliveryProtocol, deliveryBackend, hash, indexHash, idx.Length(), cfg.Average}
				attributes, modified, err := fileMetadata(f)
				if err != nil {
					return err
				}
				e := TreeEntry{Path: path, Artifact: a, Chunks: len(idx.Chunks), IndexBytes: 104 + 40*int64(len(idx.Chunks)), Mode: uint32(info.Mode().Perm()), Attributes: attributes, Modified: modified}
				cat.Entries = append(cat.Entries, e)
				pub.entries[key] = e
			}
			if err == io.EOF {
				break
			}
		}
	}
	if err := finishIndexes(s, oldIndexes, newIndexes); err != nil {
		return err
	}
	sort.Slice(cat.Entries, func(i, j int) bool { return cat.Entries[i].Path < cat.Entries[j].Path })
	blob, err := json.Marshal(cat)
	if err != nil {
		return err
	}
	token, err := newIdentity()
	if err != nil {
		return err
	}
	token2, err := newIdentity()
	if err != nil {
		return err
	}
	session, err := newIdentity()
	if err != nil {
		return err
	}
	listener, endpoint, pin, err := dataListener(cfg.Peer)
	if err != nil {
		return err
	}
	defer listener.Close()
	pub.catalog = blob
	pub.offer = TreeOffer{Protocol: deliveryProtocol, ID: sumBytes(blob), CatalogBytes: int64(len(blob)), Endpoint: endpoint, Token: token + token2, Session: session, Peer: pin}
	if err := validateCatalog(cat, pub.offer); err != nil {
		return err
	}
	server := &http.Server{Handler: pub, ReadHeaderTimeout: 5 * time.Second, ReadTimeout: 5 * time.Second, WriteTimeout: 15 * time.Minute, IdleTimeout: 5 * time.Second, MaxHeaderBytes: 4096}
	stop := func() { _ = server.Close(); pub.mu.Lock(); pub.closing = true; pub.mu.Unlock(); pub.active.Wait() }
	defer stop()
	finished := make(chan error, 1)
	go func() { finished <- server.Serve(listener) }()
	if err := durableJSON(filepath.Join(s.work, "tree-offer.json"), pub.offer); err != nil {
		return err
	}
	if ready != nil {
		if err := ready(pub.offer); err != nil {
			return err
		}
	}
	timer := time.NewTimer(time.Duration(cfg.ServeSeconds) * time.Second)
	defer timer.Stop()
	select {
	case <-release:
		stop()
		<-finished
		return nil
	case err := <-finished:
		return err
	case <-timer.C:
		stop()
		<-finished
		return nil
	}
}

func runTree(command, configPath string) error {
	d.Digest = d.SHA256{}
	var cfg TreeConfig
	if err := strictControl(configPath, &cfg); err != nil {
		return err
	}
	if cfg.Memory < 0 || cfg.Stash < 0 {
		return errors.New("invalid tree resource limits")
	}
	if cfg.Memory > 0 {
		debug.SetMemoryLimit(cfg.Memory)
	}
	if command == "tree-serve" {
		return publishTree(configPath, cfg)
	}
	if cfg.SourceRoot != "" || !validHex(cfg.Delivery, 16) {
		return errors.New("receiver may not name a source")
	}
	s, err := openSlot(filepath.Dir(configPath))
	if err != nil {
		return err
	}
	defer s.Close()
	if command == "tree-status" {
		held, err := verifyTree(s, cfg.Offer.ID, cfg.Delivery)
		closeFiles(held)
		return err
	}
	if _, err := os.Lstat(filepath.Join(s.work, "active-scenario.json")); !os.IsNotExist(err) {
		identity := cfg.ExecutorIdentity
		if identity == "" {
			identity = cfg.Peer.Identity
		}
		if identity == "" {
			return errors.New("a prior scenario needs executor identity and settlement review")
		}
		_, fingerprint, err := loadIdentity(identity)
		if err != nil {
			return err
		}
		if err := checkScenarioCustody(s, fingerprint); err != nil {
			return err
		}
	}
	if err := validTreeOffer(cfg.Offer); err != nil {
		return err
	}
	// The one installed tree never advances under another job's unresolved
	// lease on a different generation.
	if err := leasesAllow(s.work, cfg.Offer.ID, ""); err != nil {
		return err
	}
	client, err := dataClient(cfg.Offer.Endpoint, cfg.Offer.Peer, cfg.Peer)
	if err != nil {
		return err
	}
	defer client.CloseIdleConnections()
	return receiveTree(s, cfg, client)
}
