package main

// Generation leases and settled cancellation (milestone 2, Phase 3).
//
// A lease binds (project slot, generation, job) durably in the slot's
// leases/ folder, written under the slot's mutation lock before the job's
// delivery reads or advances the slot. The one installed tree is never
// advanced to another generation while a lease of another job on a
// different generation is unresolved: tree-receive refuses, and the job is
// held. Same-generation readers share. A lease is released only after its
// job's final receipt and verified whole-tree settlement; unknown,
// creation-issued, awaiting-observation and unconfirmed-cancel receipts pin
// it. Nothing is released by age.

import (
	"encoding/json"
	"errors"
	"net/http"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"time"
)

type slotLease struct {
	Protocol   int    `json:"protocol"`
	Slot       string `json:"slot"`       // slot folder relative to the executor root
	Generation string `json:"generation"` // the tree offer the job reads
	Job        string `json:"job_id"`
	Person     string `json:"person,omitempty"`
	Acquired   int64  `json:"acquired"`
}

func leasePath(base, job string) string { return filepath.Join(base, "leases", job+".json") }

// readLeases lists a slot's leases; an unreadable lease pins the slot.
func readLeases(base string) ([]slotLease, error) {
	entries, err := os.ReadDir(filepath.Join(base, "leases"))
	if os.IsNotExist(err) {
		return nil, nil
	}
	if err != nil {
		return nil, err
	}
	if len(entries) > 64 {
		return nil, errors.New("slot lease count exceeds limit")
	}
	var leases []slotLease
	for _, entry := range entries {
		if !strings.HasSuffix(entry.Name(), ".json") || strings.HasPrefix(entry.Name(), ".") {
			continue
		}
		var lease slotLease
		if _, err := readControl(filepath.Join(base, "leases", entry.Name()), &lease); err != nil {
			return nil, errors.New("slot lease needs local review")
		}
		leases = append(leases, lease)
	}
	return leases, nil
}

// leasesAllow reports whether the slot may hold generation: no other job's
// unresolved lease names a different generation.
func leasesAllow(base, generation, job string) error {
	leases, err := readLeases(base)
	if err != nil {
		return err
	}
	for _, lease := range leases {
		if lease.Job != job && lease.Generation != generation {
			return errors.New("generation-held")
		}
	}
	return nil
}

// acquireLease reserves the slot's generation for a job under the slot lock.
func acquireLease(root, relative string, lease slotLease) error {
	parents, err := managedDirectory(root, relative+"/leases")
	defer closeFiles(parents)
	if err != nil {
		return err
	}
	base := filepath.Join(root, filepath.FromSlash(relative))
	guard, err := openSlot(base)
	if err != nil {
		return err
	}
	defer guard.Close()
	if err := leasesAllow(base, lease.Generation, lease.Job); err != nil {
		return err
	}
	lease.Protocol, lease.Slot, lease.Acquired = deliveryProtocol, relative, time.Now().UnixMilli()
	return durableJSON(leasePath(base, lease.Job), lease)
}

// releaseLease removes a job's lease under the slot lock.
func releaseLease(root, relative, job string) error {
	base := filepath.Join(root, filepath.FromSlash(relative))
	guard, err := openSlot(base)
	if err != nil {
		return err
	}
	defer guard.Close()
	if err := os.Remove(leasePath(base, job)); err != nil && !os.IsNotExist(err) {
		return err
	}
	return nil
}

// finalReleasable are receipts after which a settled job's leases go.
var finalReleasable = map[string]bool{"passed": true, "failed": true, "timeout": true, "infrastructure_error": true,
	"cancelled": true, "not_run": true, "access_refused": true}

// leaseReleasable decides from the job's own receipt whether its leases may
// be released: a final receipt and a verified settled process (or none ever
// issued).
func leaseReleasable(root, job string) bool {
	var state ScenarioState
	err := readTreeControl(filepath.Join(root, "jobs", job, "job-state.json"), &state)
	if os.IsNotExist(err) {
		return false // never released without the job's own receipt
	}
	if err != nil || !finalReleasable[state.Status] {
		return false
	}
	settled, err := namedJobSettled(state.Process)
	return err == nil && settled
}

// releaseJobLeases releases a job's leases on both of its slots when its
// receipt allows it, and reports whether they were released.
func (s *executorService) releaseJobLeases(value ExecutorSubmission) bool {
	if !leaseReleasable(s.cfg.Root, value.Request.Job) && !(value.Phase == "access_refused" || value.Phase == "cancelled") {
		return false
	}
	ok := true
	for _, kind := range []string{"slots", "bundles"} {
		if err := releaseLease(s.cfg.Root, slotRelative(value, kind), value.Request.Job); err != nil {
			ok = false
		}
	}
	return ok
}

// reconcileLeases runs at start: leases of jobs whose receipts are final and
// settled are released; every other lease stays and pins its generation.
func (s *executorService) reconcileLeases() {
	for _, job := range s.queue.Jobs {
		value, err := s.readSubmission(job)
		if err != nil {
			continue
		}
		if finalReleasable[value.Phase] {
			s.releaseJobLeases(value)
		}
		// A durable cancel request survives a restart: a job that was
		// deploying when the service stopped is cancelled, never launched.
		var record cancelRecord
		if _, err := readControl(filepath.Join(s.cfg.Root, "requests", job, "cancel.json"), &record); err == nil && !finalReleasable[value.Phase] {
			cancelRequests.Store(job, record)
		}
	}
}

// --- cancellation ----------------------------------------------------------

var errCancelRequested = errors.New("cancel requested")

// cancelRequests holds in-process cancel requests by job; the durable record
// is requests/<job>/cancel.json.
var cancelRequests sync.Map

func cancelRequested(job string) bool {
	_, ok := cancelRequests.Load(job)
	return ok
}

type cancelRecord struct {
	Protocol  int    `json:"protocol"`
	RequestID string `json:"request_id"`
	Person    string `json:"person"`
	Via       string `json:"via"` // owner or operator
	At        int64  `json:"at"`
	Phase     string `json:"phase_at_request"`
}

// cancelJob handles an authorized cancel request. It never returns logs or
// results; an operator learns only the job's phase.
func (s *executorService) cancelJob(w http.ResponseWriter, r *http.Request, value ExecutorSubmission, caller *accessCaller, via string) {
	var body struct {
		RequestID string `json:"request_id"`
	}
	if decodeRequest(w, r, &body) != nil || !validUUID(body.RequestID) {
		http.Error(w, "cancel needs a request_id", http.StatusBadRequest)
		return
	}
	s.mu.Lock()
	defer s.mu.Unlock()
	path := filepath.Join(s.cfg.Root, "requests", value.Request.Job, "cancel.json")
	var prior cancelRecord
	if _, err := readControl(path, &prior); err == nil {
		current, _ := s.readSubmission(value.Request.Job)
		s.json(w, map[string]any{"job_id": value.Request.Job, "cancel": "requested", "request_id": prior.RequestID,
			"replayed": prior.RequestID == body.RequestID, "phase": current.Phase})
		return
	}
	current, err := s.readSubmission(value.Request.Job)
	if err != nil {
		http.Error(w, "unknown job", http.StatusNotFound)
		return
	}
	switch current.Phase {
	case "queued", "held", "deploying", "executing":
	default:
		w.Header().Set("Content-Type", "application/json")
		w.WriteHeader(http.StatusConflict)
		_ = json.NewEncoder(w).Encode(map[string]any{"job_id": value.Request.Job, "cancel": "not-cancellable", "phase": current.Phase})
		return
	}
	parents, err := managedDirectory(s.cfg.Root, "requests/"+value.Request.Job)
	defer closeFiles(parents)
	if err != nil {
		http.Error(w, "cancel could not be recorded", http.StatusServiceUnavailable)
		return
	}
	record := cancelRecord{deliveryProtocol, body.RequestID, caller.Person, via, time.Now().UnixMilli(), current.Phase}
	if err := durableJSON(path, record); err != nil {
		http.Error(w, "cancel could not be recorded", http.StatusServiceUnavailable)
		return
	}
	cancelRequests.Store(value.Request.Job, record)
	if current.Phase == "queued" || current.Phase == "held" {
		// Nothing was deployed or started.
		current.Phase, current.Reason = "cancelled", "cancelled by "+via+" before deployment"
		if err := s.saveSubmission(current); err != nil {
			http.Error(w, "cancel could not be recorded", http.StatusServiceUnavailable)
			return
		}
	}
	s.json(w, map[string]any{"job_id": value.Request.Job, "cancel": "requested", "request_id": body.RequestID, "replayed": false, "phase": current.Phase})
}

func validUUID(s string) bool {
	if len(s) != 36 || strings.ToLower(s) != s {
		return false
	}
	for i, c := range s {
		switch i {
		case 8, 13, 18, 23:
			if c != '-' {
				return false
			}
		default:
			if !strings.ContainsRune("0123456789abcdef", c) {
				return false
			}
		}
	}
	return true
}
