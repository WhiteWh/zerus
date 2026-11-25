package main

import (
	"encoding/binary"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"time"

	"golang.org/x/sys/windows"
)

func nativeIdentity(f *os.File) (string, error) {
	var info windows.ByHandleFileInformation
	if err := windows.GetFileInformationByHandle(windows.Handle(f.Fd()), &info); err != nil {
		return "", err
	}
	if info.FileAttributes&windows.FILE_ATTRIBUTE_REPARSE_POINT != 0 {
		return "", errors.New("receiver is a reparse point")
	}
	if info.NumberOfLinks != 1 {
		return "", errors.New("multiple file links are not supported")
	}
	return fmt.Sprintf("%08x:%08x%08x", info.VolumeSerialNumber, info.FileIndexHigh, info.FileIndexLow), nil
}

func nativeOpen(path string, access, share, disposition, flags uint32) (*os.File, error) {
	if err := localPath(path); err != nil {
		return nil, err
	}
	name, err := windows.UTF16PtrFromString(path)
	if err != nil {
		return nil, err
	}
	h, err := windows.CreateFile(name, access, share, nil, disposition,
		flags|windows.FILE_FLAG_OPEN_REPARSE_POINT, 0)
	if err != nil {
		return nil, &os.PathError{Op: fmt.Sprintf("native-open(access-%x)", access), Path: filepath.Base(path), Err: err}
	}
	f := os.NewFile(uintptr(h), path)
	if _, err := nativeIdentity(f); err != nil {
		f.Close()
		return nil, err
	}
	return f, nil
}

func localPath(path string) error {
	volume := filepath.VolumeName(path)
	if !filepath.IsAbs(path) || len(volume) != 2 || volume[1] != ':' {
		return errors.New("only absolute local drive paths are supported")
	}
	root, err := windows.UTF16PtrFromString(volume + `\`)
	if err != nil || windows.GetDriveType(root) != windows.DRIVE_FIXED {
		return errors.New("only fixed local drives are supported")
	}
	return nil
}

// Deny existing and new writers/deleters. An already-open writer prevents the
// lease rather than letting a build overwrite bytes that a delivery is serving.
func openSealed(path, expected string, writable bool) (*os.File, error) {
	access := uint32(windows.GENERIC_READ)
	if writable {
		access |= windows.GENERIC_WRITE
	}
	f, err := nativeOpen(path, access, windows.FILE_SHARE_READ, windows.OPEN_EXISTING, 0)
	if err != nil {
		return nil, err
	}
	id, err := nativeIdentity(f)
	info, statErr := f.Stat()
	if err != nil || statErr != nil || !info.Mode().IsRegular() || (expected != "" && id != expected) {
		f.Close()
		return nil, errors.New("sealed file identity changed")
	}
	return f, nil
}

// A generation may change attributes only on its already recorded native
// object. Generic read alone cannot clear READONLY through a Windows handle.
func openOwnedMetadata(path, expected string) (*os.File, error) {
	if expected == "" {
		return nil, errors.New("owned metadata identity required")
	}
	f, err := nativeOpen(path, windows.GENERIC_READ|windows.FILE_WRITE_ATTRIBUTES, windows.FILE_SHARE_READ, windows.OPEN_EXISTING, 0)
	if err != nil {
		return nil, err
	}
	id, err := nativeIdentity(f)
	info, statErr := f.Stat()
	if err != nil || statErr != nil || !info.Mode().IsRegular() || id != expected {
		f.Close()
		return nil, errors.New("owned metadata identity changed")
	}
	return f, nil
}

func lockSlot(path string) (*os.File, error) {
	f, err := nativeOpen(path, windows.GENERIC_READ|windows.GENERIC_WRITE, 0, windows.OPEN_ALWAYS, 0)
	if err != nil {
		return nil, err
	}
	info, err := f.Stat()
	if err != nil || !info.Mode().IsRegular() || info.Size() != 0 {
		f.Close()
		return nil, errors.New("invalid slot custody file")
	}
	return f, nil
}

// Hold every path component without delete sharing. This prevents an ancestor
// rename or junction replacement from redirecting later path-based desync I/O.
func holdDirectoryChain(path string) ([]*os.File, string, error) {
	volume := filepath.VolumeName(path)
	if !filepath.IsAbs(path) || len(volume) != 2 || volume[1] != ':' {
		return nil, "", errors.New("slot must use an absolute local drive path")
	}
	var names []string
	for p := filepath.Clean(path); ; p = filepath.Dir(p) {
		names = append(names, p)
		if p == filepath.Dir(p) {
			break
		}
	}
	var files []*os.File
	var id string
	for i := len(names) - 1; i >= 0; i-- {
		f, err := nativeOpen(names[i], windows.FILE_READ_ATTRIBUTES,
			windows.FILE_SHARE_READ|windows.FILE_SHARE_WRITE, windows.OPEN_EXISTING,
			windows.FILE_FLAG_BACKUP_SEMANTICS)
		if err != nil {
			return files, "", err
		}
		files = append(files, f)
		info, err := f.Stat()
		if err != nil || !info.IsDir() {
			return files, "", errors.New("invalid slot directory")
		}
		id, err = nativeIdentity(f)
		if err != nil {
			return files, "", err
		}
	}
	return files, id, nil
}

func replaceDurable(from, to string) error {
	return replaceDurableUsing(from, to, windows.MoveFileEx)
}
func replaceDurableUsing(from, to string, move func(*uint16, *uint16, uint32) error) error {
	src, err := windows.UTF16PtrFromString(from)
	if err != nil {
		return err
	}
	dst, err := windows.UTF16PtrFromString(to)
	if err != nil {
		return err
	}
	// Pin the source against writers. Legacy MoveFileEx replacement can refuse
	// an open target even with delete sharing, so target observations must close
	// before the commit. Recheck both names after every refused attempt.
	source, sourceBinding, err := replacementBinding(from)
	if err != nil {
		return err
	}
	defer source.Close()
	target, targetBinding, err := replacementBinding(to)
	if err != nil && !os.IsNotExist(err) {
		return err
	}
	if target != nil {
		target.Close()
	}
	deadline := time.Now().Add(2 * time.Second)
	for {
		if !sameReplacementBinding(from, sourceBinding) || !sameReplacementBinding(to, targetBinding) {
			return errors.New("native replacement bindings changed; commit was not replayed")
		}
		err := move(src, dst, windows.MOVEFILE_REPLACE_EXISTING|windows.MOVEFILE_WRITE_THROUGH)
		if err == nil {
			return nil
		}
		if (!errors.Is(err, windows.ERROR_ACCESS_DENIED) && !errors.Is(err, windows.ERROR_SHARING_VIOLATION) && !errors.Is(err, windows.ERROR_LOCK_VIOLATION)) || time.Now().After(deadline) {
			return &os.LinkError{Op: "native-replace", Old: filepath.Base(from), New: filepath.Base(to), Err: err}
		}
		// This retries an exactly bound, refused metadata commit only. A
		// missing/moved source or changed target stops the next attempt.
		time.Sleep(20 * time.Millisecond)
	}
}

type replacementStamp struct {
	Identity   string
	Bytes      int64
	Modified   int64
	Attributes uint32
}

func replacementBinding(path string) (*os.File, replacementStamp, error) {
	f, err := nativeOpen(path, windows.FILE_READ_ATTRIBUTES, windows.FILE_SHARE_READ|windows.FILE_SHARE_DELETE, windows.OPEN_EXISTING, 0)
	if err != nil {
		return nil, replacementStamp{}, err
	}
	id, err := nativeIdentity(f)
	info, statErr := f.Stat()
	attributes, modified, metaErr := fileMetadata(f)
	if err != nil || statErr != nil || metaErr != nil || !info.Mode().IsRegular() {
		f.Close()
		return nil, replacementStamp{}, errors.New("invalid native replacement metadata")
	}
	return f, replacementStamp{id, info.Size(), modified, attributes}, nil
}
func sameReplacementBinding(path string, expected replacementStamp) bool {
	f, actual, err := replacementBinding(path)
	if f != nil {
		f.Close()
	}
	if expected.Identity == "" {
		return os.IsNotExist(err)
	}
	return err == nil && actual == expected
}

func openReceiptNext(path string) (*os.File, error) {
	f, err := nativeOpen(path, windows.GENERIC_WRITE|windows.FILE_READ_ATTRIBUTES,
		0, windows.OPEN_ALWAYS, 0)
	if err != nil {
		return nil, err
	}
	info, err := f.Stat()
	if err != nil || !info.Mode().IsRegular() {
		f.Close()
		return nil, errors.New("invalid receipt staging object")
	}
	if err := f.Truncate(0); err != nil {
		f.Close()
		return nil, err
	}
	return f, nil
}

func removeIdentity(path, expected string) error {
	f, err := nativeOpen(path, windows.DELETE|windows.FILE_READ_ATTRIBUTES,
		windows.FILE_SHARE_READ|windows.FILE_SHARE_WRITE, windows.OPEN_EXISTING, windows.FILE_FLAG_BACKUP_SEMANTICS)
	if err != nil {
		return err
	}
	defer f.Close()
	id, err := nativeIdentity(f)
	if err != nil || id != expected {
		return errors.New("removal identity changed")
	}
	disposition := byte(1)
	return windows.SetFileInformationByHandle(windows.Handle(f.Fd()), windows.FileDispositionInfo, &disposition, 1)
}

func moveIdentity(from, to, expected string) error {
	if err := localPath(to); err != nil {
		return err
	}
	f, err := nativeOpen(from, windows.DELETE|windows.FILE_READ_ATTRIBUTES,
		windows.FILE_SHARE_READ|windows.FILE_SHARE_WRITE, windows.OPEN_EXISTING, 0)
	if err != nil {
		return err
	}
	defer f.Close()
	id, err := nativeIdentity(f)
	if err != nil || id != expected {
		return errors.New("move identity changed")
	}
	name, err := windows.UTF16FromString(to)
	if err != nil {
		return err
	}
	buffer := make([]byte, 20+2*len(name))
	binary.LittleEndian.PutUint32(buffer[16:20], uint32(2*(len(name)-1)))
	for i, c := range name {
		binary.LittleEndian.PutUint16(buffer[20+i*2:], c)
	}
	return windows.SetFileInformationByHandle(windows.Handle(f.Fd()), windows.FileRenameInfo, &buffer[0], uint32(len(buffer)))
}

func openDirectory(path string) (*os.File, error) {
	f, err := nativeOpen(path, windows.GENERIC_READ, windows.FILE_SHARE_READ|windows.FILE_SHARE_WRITE,
		windows.OPEN_EXISTING, windows.FILE_FLAG_BACKUP_SEMANTICS)
	if err != nil {
		return nil, err
	}
	info, err := f.Stat()
	if err != nil || !info.IsDir() {
		f.Close()
		return nil, errors.New("invalid managed directory")
	}
	return f, nil
}

func fileMetadata(f *os.File) (uint32, int64, error) {
	var info windows.ByHandleFileInformation
	if err := windows.GetFileInformationByHandle(windows.Handle(f.Fd()), &info); err != nil {
		return 0, 0, err
	}
	return info.FileAttributes & 0x27, int64(uint64(info.LastWriteTime.HighDateTime)<<32 | uint64(info.LastWriteTime.LowDateTime)), nil
}

func privateDirectory(path string) error {
	user, err := windows.GetCurrentProcessToken().GetTokenUser()
	if err != nil {
		return err
	}
	sddl := "D:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;OICI;FA;;;" + user.User.Sid.String() + ")"
	descriptor, err := windows.SecurityDescriptorFromString(sddl)
	if err != nil {
		return err
	}
	dacl, _, err := descriptor.DACL()
	if err != nil {
		return err
	}
	return windows.SetNamedSecurityInfo(path, windows.SE_FILE_OBJECT,
		windows.DACL_SECURITY_INFORMATION|windows.PROTECTED_DACL_SECURITY_INFORMATION, nil, nil, dacl, nil)
}

func removeGenerated(path string, object GeneratedObject) error {
	f, err := nativeOpen(path, windows.DELETE|windows.FILE_READ_ATTRIBUTES|windows.FILE_WRITE_ATTRIBUTES,
		windows.FILE_SHARE_READ, windows.OPEN_EXISTING, windows.FILE_FLAG_BACKUP_SEMANTICS)
	if err != nil {
		return err
	}
	defer f.Close()
	id, err := nativeIdentity(f)
	if err != nil || id != object.Identity {
		return errors.New("generated cleanup identity changed")
	}
	info, err := f.Stat()
	if err != nil || info.IsDir() != object.Directory {
		return errors.New("generated cleanup type changed")
	}
	if !object.Directory {
		attributes, modified, err := fileMetadata(f)
		if err != nil || info.Size() != object.Bytes || modified != object.Modified {
			return errors.New("changed generated content is preserved for review")
		}
		if attributes&windows.FILE_ATTRIBUTE_READONLY != 0 {
			var basic [40]byte
			attributes &^= windows.FILE_ATTRIBUTE_READONLY
			if attributes == 0 {
				attributes = windows.FILE_ATTRIBUTE_NORMAL
			}
			binary.LittleEndian.PutUint32(basic[32:36], attributes)
			if err := windows.SetFileInformationByHandle(windows.Handle(f.Fd()), windows.FileBasicInfo, &basic[0], uint32(len(basic))); err != nil {
				return err
			}
		}
	}
	flag := byte(1)
	return windows.SetFileInformationByHandle(windows.Handle(f.Fd()), windows.FileDispositionInfo, &flag, 1)
}

func applyFileMetadata(f *os.File, attributes uint32, modified int64) error {
	if attributes&^uint32(0x27) != 0 || modified <= 0 {
		return errors.New("unsupported file metadata")
	}
	var basic [40]byte
	binary.LittleEndian.PutUint64(basic[16:24], uint64(modified))
	if attributes == 0 {
		attributes = windows.FILE_ATTRIBUTE_NORMAL
	}
	binary.LittleEndian.PutUint32(basic[32:36], attributes)
	return windows.SetFileInformationByHandle(windows.Handle(f.Fd()), windows.FileBasicInfo, &basic[0], uint32(len(basic)))
}

// Keep the exact receiver object from being renamed/replaced while desync opens
// additional read/write handles. No delete sharing is granted.
func holdReceiver(path, expected string) (*os.File, error) {
	name, err := windows.UTF16PtrFromString(path)
	if err != nil {
		return nil, err
	}
	h, err := windows.CreateFile(name, windows.FILE_READ_ATTRIBUTES,
		windows.FILE_SHARE_READ|windows.FILE_SHARE_WRITE, nil, windows.OPEN_EXISTING,
		windows.FILE_FLAG_OPEN_REPARSE_POINT, 0)
	if err != nil {
		return nil, err
	}
	f := os.NewFile(uintptr(h), path)
	id, err := nativeIdentity(f)
	if err != nil || id != expected {
		f.Close()
		return nil, errors.New("receiver identity changed")
	}
	return f, nil
}

// The parent assigns the owned child to its hard memory-limited Job Object
// before allowing any configuration or proprietary input to be read.
func qualificationGate() error {
	name := os.Getenv("ZERUS_DESYNC_QUALIFICATION_GATE")
	if name == "" {
		return nil
	}
	ptr, err := windows.UTF16PtrFromString(name)
	if err != nil {
		return err
	}
	h, err := windows.OpenEvent(windows.SYNCHRONIZE, false, ptr)
	if err != nil {
		return err
	}
	defer windows.CloseHandle(h)
	status, err := windows.WaitForSingleObject(h, 30000)
	if err != nil || status != windows.WAIT_OBJECT_0 {
		return errors.New("qualification gate did not open")
	}
	return nil
}
