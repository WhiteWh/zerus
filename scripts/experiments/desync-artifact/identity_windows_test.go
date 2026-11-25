package main

import (
	"golang.org/x/sys/windows"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"
)

func TestDurableMetadataReplacementWaitsForRefusedReaderOnly(t *testing.T) {
	for _, changeSource := range []bool{false, true} {
		dir := t.TempDir()
		from, to := filepath.Join(dir, "record.next"), filepath.Join(dir, "record.json")
		if err := os.WriteFile(from, []byte("new synthetic record"), 0600); err != nil {
			t.Fatal(err)
		}
		if err := os.WriteFile(to, []byte("old synthetic record"), 0600); err != nil {
			t.Fatal(err)
		}
		reader, err := nativeOpen(to, windows.GENERIC_READ, windows.FILE_SHARE_READ, windows.OPEN_EXISTING, 0)
		if err != nil {
			t.Fatal(err)
		}
		finished := make(chan error, 1)
		refused := make(chan struct{}, 1)
		go func() {
			finished <- replaceDurableUsing(from, to, func(src, dst *uint16, flags uint32) error {
				err := windows.MoveFileEx(src, dst, flags)
				if err != nil {
					select {
					case refused <- struct{}{}:
					default:
					}
				}
				return err
			})
		}()
		select {
		case <-refused:
		case <-time.After(time.Second):
			reader.Close()
			<-finished
			t.Fatal("native refusal was not observed")
		}
		if changeSource {
			if err := os.Rename(from, from+".moved"); err != nil {
				reader.Close()
				t.Fatal(err)
			}
			if err := os.WriteFile(from, []byte("unrelated synthetic record"), 0600); err != nil {
				reader.Close()
				t.Fatal(err)
			}
		}
		reader.Close()
		err = <-finished
		if changeSource {
			if err == nil {
				t.Fatal("changed source was committed after a refusal")
			}
			b, _ := os.ReadFile(to)
			if string(b) != "old synthetic record" {
				t.Fatal("unrelated replacement changed the destination")
			}
		} else {
			if err != nil {
				t.Fatal(err)
			}
			b, _ := os.ReadFile(to)
			if string(b) != "new synthetic record" {
				t.Fatal("durable metadata did not settle")
			}
		}
	}
}

func TestNativeMovesPreserveIdentityAcrossPathLengths(t *testing.T) {
	root := t.TempDir()
	for _, length := range []int{1, 2, 3, 7, 8, 15, 31, 64, 127} {
		parent := filepath.Join(root, strings.Repeat("n", length))
		if err := os.Mkdir(parent, 0700); err != nil {
			t.Fatal(err)
		}
		from := filepath.Join(root, "staging.bin")
		to := filepath.Join(parent, "destination-é.bin")
		f, err := os.OpenFile(from, os.O_CREATE|os.O_EXCL|os.O_RDWR, 0600)
		if err != nil {
			t.Fatal(err)
		}
		id, err := nativeIdentity(f)
		f.Close()
		if err != nil {
			t.Fatal(err)
		}
		if err := moveIdentity(from, to, id); err != nil {
			t.Fatal(err)
		}
		f, err = openSealed(to, id, false)
		if err != nil {
			t.Fatal(err)
		}
		f.Close()
		if err := removeIdentity(to, id); err != nil {
			t.Fatal(err)
		}
	}
}

func TestNativeMoveNeverReplacesAnUnrelatedObject(t *testing.T) {
	root := t.TempDir()
	from := filepath.Join(root, "from")
	to := filepath.Join(root, "to")
	if err := os.WriteFile(from, []byte("owned synthetic data"), 0600); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(to, []byte("unrelated synthetic data"), 0600); err != nil {
		t.Fatal(err)
	}
	f, err := openSealed(from, "", false)
	if err != nil {
		t.Fatal(err)
	}
	id, err := nativeIdentity(f)
	f.Close()
	if err != nil {
		t.Fatal(err)
	}
	if err := moveIdentity(from, to, id); err == nil {
		t.Fatal("existing destination was replaced")
	}
	b, err := os.ReadFile(to)
	if err != nil || string(b) != "unrelated synthetic data" {
		t.Fatal("unrelated destination changed")
	}
}
