package main

import (
	"errors"
	"os"
	"path/filepath"
	"strings"
)

func portablePath(path string) bool {
	if path == "" || len(path) > 512 || strings.ContainsAny(path, `\:*?"<>|`) {
		return false
	}
	for _, c := range path {
		if c < 32 || c == 127 {
			return false
		}
	}
	for _, part := range strings.Split(path, "/") {
		if part == "" || part == "." || part == ".." || strings.TrimRight(part, ". ") != part {
			return false
		}
		base := strings.ToUpper(strings.SplitN(part, ".", 2)[0])
		if base == "CON" || base == "PRN" || base == "AUX" || base == "NUL" ||
			(len(base) == 4 && (strings.HasPrefix(base, "COM") || strings.HasPrefix(base, "LPT")) && base[3] >= '0' && base[3] <= '9') {
			return false
		}
	}
	return true
}

func receiverPath(s *slot, m Manifest) (string, error) {
	if m.TreePath == "" {
		return filepath.Join(s.work, "receiver.bin"), nil
	}
	if !portablePath(m.TreePath) || filepath.Base(filepath.Dir(s.work)) != "files" ||
		filepath.Base(s.work) != sumBytes([]byte(m.TreePath)) {
		return "", errors.New("invalid managed tree path")
	}
	root := filepath.Dir(filepath.Dir(s.work))
	return filepath.Join(root, "build", filepath.FromSlash(m.TreePath)), nil
}

func offerBase(o Offer) string {
	if o.Resource == "" {
		return o.Endpoint
	}
	return o.Endpoint + "/tree/v1/files/" + o.Resource
}

// Each component is checked and held before its child is created. Existing
// junctions and links never grant a route outside this managed tree.
func managedDirectory(root, relative string) ([]*os.File, error) {
	parents, _, err := holdDirectoryChain(root)
	if err != nil {
		return parents, err
	}
	path := root
	if relative == "" || relative == "." {
		return parents, nil
	}
	if !portablePath(filepath.ToSlash(relative)) {
		return parents, errors.New("invalid directory path")
	}
	for _, component := range strings.Split(filepath.ToSlash(relative), "/") {
		path = filepath.Join(path, component)
		if err := os.Mkdir(path, 0700); err != nil && !os.IsExist(err) {
			return parents, err
		}
		more, _, err := holdDirectoryChain(path)
		parents = append(parents, more...)
		if err != nil {
			return parents, err
		}
	}
	return parents, nil
}

func closeFiles(files []*os.File) {
	for i := len(files) - 1; i >= 0; i-- {
		_ = files[i].Close()
	}
}
