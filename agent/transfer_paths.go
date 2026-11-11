package main

import (
	"crypto/rand"
	"fmt"
	"io"
	"io/fs"
	"os"
	"path"
	"path/filepath"
	"strings"

	"github.com/pkg/sftp"
)

// A tree path is always a slash-separated path relative to the selected root.
func remoteTreePath(root, relative string) (string, error) {
	if !fs.ValidPath(relative) || strings.ContainsAny(relative, "\\:\x00") {
		return "", fmt.Errorf("invalid relative transfer path %q", relative)
	}
	if relative == "." {
		return root, nil
	}
	for _, component := range strings.Split(relative, "/") {
		// Windows servers can normalize a component made of dots and spaces.
		if strings.TrimRight(component, ". ") == "" {
			return "", fmt.Errorf("invalid relative transfer path %q", relative)
		}
	}
	return path.Join(root, relative), nil
}

func localTreePath(relative string) (string, error) {
	name, err := filepath.Localize(relative)
	if err != nil {
		return "", fmt.Errorf("invalid local transfer path %q: %w", relative, err)
	}
	return name, nil
}

type remoteTreeReader interface {
	Lstat(string) (os.FileInfo, error)
	ReadDir(string) ([]os.FileInfo, error)
}

func walkRemoteTree(client remoteTreeReader, root string, visit func(string, string, os.FileInfo) error) error {
	info, err := client.Lstat(root)
	if err != nil {
		return err
	}
	if !info.IsDir() {
		return fmt.Errorf("remote tree root is not a directory")
	}
	var walk func(string, string, os.FileInfo, int) error
	walk = func(remote, relative string, info os.FileInfo, depth int) error {
		if !info.IsDir() && !info.Mode().IsRegular() {
			return nil // Skip symlinks and special files.
		}
		if err := visit(remote, relative, info); err != nil || !info.IsDir() {
			return err
		}
		if depth >= 256 {
			return fmt.Errorf("remote folder nesting exceeds the transfer limit")
		}
		entries, err := client.ReadDir(remote)
		if err != nil {
			return err
		}
		for _, entry := range entries {
			name := entry.Name()
			if name == "." || !fs.ValidPath(name) || strings.ContainsAny(name, "/\\\x00") {
				return fmt.Errorf("invalid remote directory entry %q", name)
			}
			if err := walk(path.Join(remote, name), path.Join(relative, name), entry, depth+1); err != nil {
				return err
			}
		}
		return nil
	}
	return walk(root, ".", info, 0)
}

func openDownloadRoot(local string) (*os.Root, error) {
	if err := os.MkdirAll(local, 0o755); err != nil {
		return nil, err
	}
	return os.OpenRoot(local)
}

func copyRemoteToRoot(client *sftp.Client, remote string, root *os.Root, local string) error {
	info, err := client.Lstat(remote)
	if err != nil {
		return err
	}
	if !info.Mode().IsRegular() {
		return fmt.Errorf("remote file is not a regular file: %s", remote)
	}
	src, err := client.Open(remote)
	if err != nil {
		return err
	}
	defer src.Close()
	return writeTreeFile(root, local, src, info)
}

func writeTreeFile(root *os.Root, local string, src io.Reader, info os.FileInfo) error {
	if err := root.MkdirAll(filepath.Dir(local), 0o755); err != nil {
		return err
	}
	// Replace the directory entry instead of truncating a possibly hard-linked
	// destination. Only publish a file after the copy finishes.
	tmp := ".openterm-download-" + rand.Text()
	dst, err := root.OpenFile(tmp, os.O_WRONLY|os.O_CREATE|os.O_EXCL, 0o666)
	if err != nil {
		return err
	}
	defer root.Remove(tmp)
	_, copyErr := io.Copy(dst, src)
	if copyErr == nil && info != nil {
		preserveDownloadTime(dst, info.ModTime())
	}
	closeErr := dst.Close()
	if copyErr != nil {
		return copyErr
	}
	if closeErr != nil {
		return closeErr
	}
	return root.Rename(tmp, local)
}
