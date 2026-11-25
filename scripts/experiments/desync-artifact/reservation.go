package main

import (
	"errors"
	"os"
	"path/filepath"
)

// Place a new empty object only after its native identity has been persisted.
// No payload write relies on a reservation name as proof of ownership.
type receiverReservation struct {
	Protocol int    `json:"protocol"`
	Root     string `json:"root_identity"`
	Delivery string `json:"delivery_id"`
	Artifact string `json:"artifact_id"`
	TreePath string `json:"tree_path"`
	Receiver string `json:"receiver_identity"`
}

func reserveReceiver(s *slot, cfg ReceiveConfig, target string) (*os.File, error) {
	var reservation receiverReservation
	path := filepath.Join(s.work, "reservation.json")
	stage := filepath.Join(s.work, "seed.new")
	if _, err := readControl(path, &reservation); os.IsNotExist(err) {
		if _, err := os.Lstat(target); !os.IsNotExist(err) {
			return nil, errors.New("existing receiver needs explicit native identity adoption")
		}
		f, err := os.OpenFile(stage, os.O_CREATE|os.O_EXCL|os.O_RDWR, 0600)
		if err != nil {
			return nil, errors.New("new receiver reservation unavailable; preserve unknown objects for review")
		}
		id, idErr := nativeIdentity(f)
		syncErr, closeErr := f.Sync(), f.Close()
		if idErr != nil || syncErr != nil || closeErr != nil {
			return nil, errors.New("receiver reservation failed")
		}
		reservation = receiverReservation{deliveryProtocol, s.root, cfg.Delivery, cfg.Offer.ID, cfg.TreePath, id}
		if err := durableJSON(path, reservation); err != nil {
			return nil, err
		}
	} else if err != nil {
		return nil, err
	}
	if reservation.Protocol != deliveryProtocol || reservation.Root != s.root || reservation.Delivery != cfg.Delivery || reservation.Artifact != cfg.Offer.ID || reservation.TreePath != cfg.TreePath || reservation.Receiver == "" {
		return nil, errors.New("receiver reservation identity changed")
	}
	if f, err := openSealed(target, reservation.Receiver, true); err == nil {
		return f, nil
	} else if !os.IsNotExist(err) {
		return nil, err
	}
	if err := moveIdentity(stage, target, reservation.Receiver); err != nil {
		return nil, err
	}
	return openSealed(target, reservation.Receiver, true)
}
