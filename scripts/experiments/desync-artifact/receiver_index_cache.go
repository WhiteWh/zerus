package main

import (
	"bytes"
	"encoding/json"
	"errors"
	"os"
	"path/filepath"

	d "github.com/folbricht/desync"
)

// The receiver keeps the chunk index of the file it last verified, so the
// next delivery of the same path seeds from it instead of reading the whole
// file again (30 s per 5 GiB on the LAN qualification host). The index is
// keyed by the file's native identity and size. It is only a seed: the final
// SHA-256 verification still reads the result, and a delivery that used a
// stale cached index is redone from a freshly computed one.
const (
	receiverIndexFile = "receiver-index.caibx"
	receiverIndexMeta = "receiver-index.json"
)

type receiverIndexStamp struct {
	Identity string `json:"identity"`
	Size     int64  `json:"size"`
	Average  uint64 `json:"average"`
	Index    string `json:"index_sha256"`
}

func receiverStamp(f *os.File) (string, int64, error) {
	id, err := nativeIdentity(f)
	if err != nil {
		return "", 0, err
	}
	info, err := f.Stat()
	if err != nil {
		return "", 0, err
	}
	return id, info.Size(), nil
}

// loadReceiverIndex returns the cached index when it was saved for this very
// file (same identity and size) and chunking parameters.
func loadReceiverIndex(work string, custody *os.File, average uint64) (d.Index, bool) {
	var stamp receiverIndexStamp
	meta, err := os.ReadFile(filepath.Join(work, receiverIndexMeta))
	if err != nil || json.Unmarshal(meta, &stamp) != nil {
		return d.Index{}, false
	}
	id, size, err := receiverStamp(custody)
	if err != nil || stamp.Identity != id || stamp.Size != size || stamp.Average != average {
		return d.Index{}, false
	}
	blob, err := os.ReadFile(filepath.Join(work, receiverIndexFile))
	if err != nil || sumBytes(blob) != stamp.Index {
		return d.Index{}, false
	}
	idx, err := d.IndexFromReader(bytes.NewReader(blob))
	if err != nil || idx.Length() != size {
		return d.Index{}, false
	}
	return idx, true
}

// saveReceiverIndex records idx as the exact index of the verified file f.
func saveReceiverIndex(work string, f *os.File, average uint64, idx d.Index) error {
	id, size, err := receiverStamp(f)
	if err != nil {
		return err
	}
	if idx.Length() != size {
		return errors.New("index does not describe the verified file")
	}
	var blob bytes.Buffer
	if _, err := idx.WriteTo(&blob); err != nil {
		return err
	}
	_ = os.Remove(filepath.Join(work, receiverIndexMeta))
	if err := durableBytes(filepath.Join(work, receiverIndexFile), blob.Bytes()); err != nil {
		return err
	}
	return durableJSON(filepath.Join(work, receiverIndexMeta), receiverIndexStamp{id, size, average, sumBytes(blob.Bytes())})
}

func forgetReceiverIndex(work string) {
	_ = os.Remove(filepath.Join(work, receiverIndexMeta))
}

// sameChunks reports whether two indexes describe identical content layouts.
func sameChunks(a, b d.Index) bool {
	if len(a.Chunks) != len(b.Chunks) || a.Length() != b.Length() {
		return false
	}
	for i := range a.Chunks {
		if a.Chunks[i].ID != b.Chunks[i].ID || a.Chunks[i].Start != b.Chunks[i].Start || a.Chunks[i].Size != b.Chunks[i].Size {
			return false
		}
	}
	return true
}
