package main

import (
	"os"
	"os/exec"
	"path/filepath"
	"runtime"
	"strings"
	"testing"
)

func TestTreePathsRequireRelativeNames(t *testing.T) {
	for _, name := range []string{"", "../other", "/absolute", "folder/../file", "folder//file", "bad\x00name"} {
		if _, err := remoteTreePath("/root", name); err == nil {
			t.Errorf("accepted invalid remote tree path %q", name)
		}
		if _, err := localTreePath(name); err == nil {
			t.Errorf("accepted invalid local tree path %q", name)
		}
	}
	for _, name := range []string{"C:relative", "nested/.. /file.txt"} {
		if _, err := remoteTreePath(".", name); err == nil {
			t.Errorf("accepted platform-dependent relative path %q", name)
		}
	}
	if got, err := remoteTreePath("/root", "nested/file.txt"); err != nil || got != "/root/nested/file.txt" {
		t.Fatalf("valid path: %q, %v", got, err)
	}
}

func TestTreeWritePreservesExistingHardLinkTarget(t *testing.T) {
	dir := t.TempDir()
	outside := filepath.Join(dir, "original.txt")
	if err := os.WriteFile(outside, []byte("original"), 0o600); err != nil {
		t.Fatal(err)
	}
	folder := filepath.Join(dir, "download")
	root, err := openDownloadRoot(folder)
	if err != nil {
		t.Fatal(err)
	}
	defer root.Close()
	if err := os.Link(outside, filepath.Join(folder, "file.txt")); err != nil {
		t.Fatal(err)
	}
	if err := writeTreeFile(root, "file.txt", strings.NewReader("download"), nil); err != nil {
		t.Fatal(err)
	}
	if original, err := os.ReadFile(outside); err != nil || string(original) != "original" {
		t.Fatalf("original file changed: %q, %v", original, err)
	}
	if local, err := root.ReadFile("file.txt"); err != nil || string(local) != "download" {
		t.Fatalf("downloaded file: %q, %v", local, err)
	}
}

func TestTreeWriteRejectsDirectoryLinkOutsideRoot(t *testing.T) {
	folder := t.TempDir()
	outside := t.TempDir()
	link := filepath.Join(folder, "link")
	if err := os.Symlink(outside, link); err != nil {
		if runtime.GOOS != "windows" {
			t.Fatal(err)
		}
		// A junction exercises the directory-link boundary without requiring
		// Windows Developer Mode or symlink creation privileges.
		command := exec.Command("powershell.exe", "-NoProfile", "-NonInteractive", "-Command",
			"$ErrorActionPreference = 'Stop'; New-Item -ItemType Junction -Path $env:OTM_TEST_LINK -Target $env:OTM_TEST_TARGET")
		command.Env = append(os.Environ(), "OTM_TEST_LINK="+link, "OTM_TEST_TARGET="+outside)
		if output, err := command.CombinedOutput(); err != nil {
			t.Fatalf("create directory junction: %v: %s", err, output)
		}
	}
	root, err := os.OpenRoot(folder)
	if err != nil {
		t.Fatal(err)
	}
	defer root.Close()
	if err := writeTreeFile(root, filepath.Join("link", "file.txt"), strings.NewReader("test"), nil); err == nil {
		t.Fatal("tree write escaped the download root")
	}
	if _, err := os.Stat(filepath.Join(outside, "file.txt")); !os.IsNotExist(err) {
		t.Fatalf("outside file should not exist: %v", err)
	}
}
