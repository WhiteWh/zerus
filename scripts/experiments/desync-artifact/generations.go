package main

import (
	"errors"
	"net/http"
	"os"
	"path/filepath"
)

// A bounded, durable intent bridges replacement of the three small metadata
// records. It makes both generations unavailable before any payload is changed.
type generationIntent struct {
	Protocol         int      `json:"protocol"`
	Previous         string   `json:"previous_artifact_id"`
	PreviousDelivery string   `json:"previous_delivery_id"`
	PreviousManifest string   `json:"previous_manifest_sha256"`
	Next             Manifest `json:"next"`
}

func advanceGeneration(s *slot, cfg ReceiveConfig, old Manifest, digest string, client *http.Client) error {
	if cfg.Previous != old.ArtifactID || !validHex(cfg.Previous, 32) || cfg.Delivery == old.DeliveryID ||
		old.Source != "publication:"+old.ArtifactID {
		return errors.New("generation change needs the exact previous artifact and a new delivery")
	}
	var f *os.File
	var err error
	if cfg.TreePath != "" && cfg.TreePath == old.TreePath {
		// The enclosing tree intent has already pinned the complete owned
		// inventory. Corrupt bytes remain a basis, never a ready old artifact.
		if _, err = s.receipt(old, digest); err != nil {
			return err
		}
		target, e := receiverPath(s, old)
		if e != nil {
			return e
		}
		f, err = openSealed(target, old.Receiver, false)
	} else {
		f, err = s.ready(old, digest)
	}
	if err != nil {
		return err
	}
	defer f.Close()
	if err := downloadIndexAs(s, cfg.Offer, client, "next.caibx"); err != nil {
		return err
	}
	next := old
	next.Artifact, next.ArtifactID = cfg.Offer.Artifact, cfg.Offer.ID
	next.DeliveryID, next.Source = cfg.Delivery, "publication:"+cfg.Offer.ID
	next.Expected, next.TargetBytes, next.Chunks = cfg.Offer.Artifact.SHA256, cfg.Offer.Artifact.Bytes, cfg.Offer.Chunks
	next.Config = Config{Average: cfg.Offer.Artifact.Average}
	intent := generationIntent{deliveryProtocol, old.ArtifactID, old.DeliveryID, digest, next}
	if err := durableJSON(filepath.Join(s.work, "generation-intent.json"), intent); err != nil {
		return err
	}
	crashCheckpoint(Config{CrashPhase: cfg.CrashPhase}, "generation_intent")
	return finishGeneration(s, cfg)
}

func finishGeneration(s *slot, cfg ReceiveConfig) error {
	var intent generationIntent
	path := filepath.Join(s.work, "generation-intent.json")
	if err := strictControl(path, &intent); err != nil {
		return err
	}
	m := intent.Next
	if intent.Protocol != deliveryProtocol || intent.Previous != cfg.Previous ||
		!validHex(intent.PreviousManifest, 32) || !validHex(intent.PreviousDelivery, 16) ||
		m.Protocol != deliveryProtocol || m.Root != s.root || m.ArtifactID != cfg.Offer.ID ||
		m.Artifact != cfg.Offer.Artifact || m.DeliveryID != cfg.Delivery ||
		m.Source != "publication:"+cfg.Offer.ID || m.Chunks != cfg.Offer.Chunks ||
		m.Destination == "" || m.Receiver == "" || m.Expected != cfg.Offer.Artifact.SHA256 ||
		m.TargetBytes != cfg.Offer.Artifact.Bytes || m.Config.Average != cfg.Offer.Artifact.Average {
		return errors.New("pending generation identity changed")
	}
	target, err := receiverPath(s, m)
	if err != nil {
		return err
	}
	custody, err := holdReceiver(target, m.Receiver)
	if err != nil {
		return err
	}
	defer custody.Close()
	nextPath := filepath.Join(s.work, "next.caibx")
	if f, err := openSealed(nextPath, "", false); err == nil {
		_, checkErr := checkedIndex(f, cfg.Offer)
		f.Close()
		if checkErr != nil {
			return checkErr
		}
		if err := replaceDurable(nextPath, filepath.Join(s.work, "target.caibx")); err != nil {
			return err
		}
	} else if os.IsNotExist(err) {
		f, err := openSealed(filepath.Join(s.work, "target.caibx"), "", false)
		if err != nil {
			return err
		}
		_, checkErr := checkedIndex(f, cfg.Offer)
		f.Close()
		if checkErr != nil {
			return checkErr
		}
	} else {
		return err
	}
	crashCheckpoint(Config{CrashPhase: cfg.CrashPhase}, "generation_index")
	if err := durableJSON(filepath.Join(s.work, "manifest.json"), m); err != nil {
		return err
	}
	crashCheckpoint(Config{CrashPhase: cfg.CrashPhase}, "generation_manifest")
	_, digest, err := s.manifest()
	if err != nil {
		return err
	}
	r := receiptFor(m, digest)
	if err := s.transition(&r, "prepared"); err != nil {
		return err
	}
	crashCheckpoint(Config{CrashPhase: cfg.CrashPhase}, "generation_receipt")
	return os.Remove(path)
}
