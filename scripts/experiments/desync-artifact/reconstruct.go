// One reconstruction path for combined and separated qualification modes.
package main

import (
	"context"
	"errors"
	"net"
	"net/url"
	"os"
	"path/filepath"
	"runtime"
	"time"

	d "github.com/folbricht/desync"
)

// transportScope labels a result with the transport its bytes actually used,
// from the offer endpoint and pin that dataClient already validated. The
// combined local mode and the token-authenticated plain loopback mode keep the
// historical "authenticated-loopback-only" label; a paired TLS peer is
// "paired-tls-loopback" on 127.0.0.0/8 and "paired-tls-private-network" (with
// external_network true) on any other private IPv4 address. Anything this
// cannot classify is reported as unverified, never as loopback.
func transportScope(endpoint, pin string) map[string]any {
	u, err := url.Parse(endpoint)
	if err != nil || u.Host == "" {
		return map[string]any{"transport": "unverified", "external_network": nil}
	}
	host, _, err := net.SplitHostPort(u.Host)
	ip := net.ParseIP(host)
	if err != nil || ip == nil || ip.To4() == nil {
		return map[string]any{"transport": "unverified", "external_network": nil}
	}
	switch {
	case pin == "" && u.Scheme == "http" && ip.IsLoopback():
		return map[string]any{"transport": "authenticated-loopback-only", "external_network": false}
	case pin != "" && u.Scheme == "https" && ip.IsLoopback():
		return map[string]any{"transport": "paired-tls-loopback", "external_network": false}
	case pin != "" && u.Scheme == "https" && ip.IsPrivate():
		return map[string]any{"transport": "paired-tls-private-network", "external_network": true}
	}
	return map[string]any{"transport": "unverified", "external_network": nil}
}

func withScope(scope, values map[string]any) map[string]any {
	for key, value := range scope {
		values[key] = value
	}
	return values
}

func reconstruct(slot *slot, manifest Manifest, digest string, cfg Config, idx d.Index,
	store d.Store, checkSource func() error, metrics func() map[string]any) error {
	target, err := receiverPath(slot, manifest)
	if err != nil {
		return err
	}
	parents, _, err := holdDirectoryChain(filepath.Dir(target))
	defer func() {
		for _, f := range parents {
			f.Close()
		}
	}()
	if err != nil {
		return err
	}
	work := slot.work
	receipt, err := slot.receipt(manifest, digest)
	if err != nil {
		return err
	}
	custody, err := holdReceiver(target, manifest.Receiver)
	if err != nil {
		return err
	}
	defer custody.Close()
	if err := slot.transition(&receipt, "applying"); err != nil {
		return err
	}
	if manifest.TreePath != "" {
		if err := os.Chmod(target, 0600); err != nil {
			return err
		}
	}
	crashCheckpoint(cfg, "before_apply")
	indexStart := time.Now()
	old, cached := loadReceiverIndex(work, custody, cfg.Average)
	// The cache is only a seed; it is consumed once and rewritten after the
	// next verified result.
	forgetReceiverIndex(work)
	if !cached {
		old, _, err = d.IndexFromFile(context.Background(), target, 1, cfg.Average/4, cfg.Average, cfg.Average*4, d.NullProgressBar{})
		if err != nil {
			return err
		}
	}
	receiverIndexSeconds := time.Since(indexStart).Seconds()
	assemble := func(old d.Index) (*d.ExtractStats, error) {
		if cached && sameChunks(old, idx) {
			// The file already holds exactly these chunks: nothing to write.
			return &d.ExtractStats{ChunksInPlace: uint64(len(idx.Chunks)), ChunksTotal: len(idx.Chunks), BytesTotal: idx.Length(), Seeds: 1}, nil
		}
		seed, err := d.NewFileSeed(target, target, old)
		if err != nil {
			return nil, err
		}
		return d.AssembleFile(context.Background(), target, idx, store, []d.Seed{seed},
			d.AssembleOptions{N: 1, StashLimitBytes: &cfg.StashBytes})
	}
	start := time.Now()
	stats, applyErr := assemble(old)
	applySeconds := time.Since(start).Seconds()
	result := map[string]any{
		"ok": false, "apply_seconds": applySeconds, "receiver_index_seconds": receiverIndexSeconds,
		"receiver_index_cached": cached,
		"statistics": stats,
		"transport":  "authenticated-loopback-only", "external_network": false,
		"stash_limit_bytes": cfg.StashBytes,
	}
	for key, value := range metrics() {
		result[key] = value
	}
	var mem runtime.MemStats
	runtime.ReadMemStats(&mem)
	result["go_heap_bytes"] = mem.HeapAlloc
	result["go_sys_bytes"] = mem.Sys
	redo := func() error {
		// A stale cached seed: recompute the index from the file and assemble again.
		fresh, _, err := d.IndexFromFile(context.Background(), target, 1, cfg.Average/4, cfg.Average, cfg.Average*4, d.NullProgressBar{})
		if err != nil {
			return err
		}
		cached = false
		stats, applyErr = assemble(fresh)
		result["statistics"], result["receiver_index_cached"], result["receiver_index_redone"] = stats, false, true
		return nil
	}
	if applyErr != nil && cached {
		if err := redo(); err != nil {
			return err
		}
	}
	if applyErr != nil {
		result["error"] = "chunk transfer or reconstruction refused"
		result["private_diagnostic"] = applyErr.Error()
		durableJSON(filepath.Join(work, "result.json"), result)
		return errors.New("apply failed; receiver unavailable")
	}
	crashCheckpoint(cfg, "after_apply")
	if err := slot.transition(&receipt, "verifying"); err != nil {
		return err
	}
	out, err := openSealed(target, manifest.Receiver, true)
	if err != nil {
		return err
	}
	defer out.Close()
	err = out.Sync()
	if err != nil {
		return err
	}
	crashCheckpoint(cfg, "after_flush")
	verifyErr := verifyHandle(out, manifest)
	if verifyErr != nil && cached {
		out.Close()
		if err := redo(); err != nil {
			return err
		}
		if applyErr != nil {
			result["error"] = "chunk transfer or reconstruction refused"
			result["private_diagnostic"] = applyErr.Error()
			durableJSON(filepath.Join(work, "result.json"), result)
			return errors.New("apply failed; receiver unavailable")
		}
		if out, err = openSealed(target, manifest.Receiver, true); err != nil {
			return err
		}
		defer out.Close()
		if err := out.Sync(); err != nil {
			return err
		}
		verifyErr = verifyHandle(out, manifest)
	}
	if verifyErr != nil {
		result["error"] = "final content mismatch"
		durableJSON(filepath.Join(work, "result.json"), result)
		return errors.New("final verification failed")
	}
	if err := checkSource(); err != nil {
		return err
	}
	result["ok"] = true
	result["exact_hash_match"] = true
	// Best effort: a missing cache only costs the next delivery a full index.
	_ = saveReceiverIndex(work, out, cfg.Average, idx)
	if err := durableJSON(filepath.Join(work, "result.json"), result); err != nil {
		return err
	}
	crashCheckpoint(cfg, "before_ready")
	if err := slot.transition(&receipt, "ready"); err != nil {
		return err
	}
	crashCheckpoint(cfg, "after_ready")
	return nil
}
