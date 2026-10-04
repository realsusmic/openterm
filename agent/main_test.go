package main

import (
	"bufio"
	"bytes"
	"crypto/ed25519"
	"crypto/rand"
	"encoding/base64"
	"net"
	"os"
	"path/filepath"
	"sync/atomic"
	"testing"
	"time"

	"golang.org/x/crypto/ssh"
	"golang.org/x/crypto/ssh/knownhosts"
)

func TestLocalManifestMetadataAndSyncPlan(t *testing.T) {
	root := t.TempDir()
	if err := os.MkdirAll(filepath.Join(root, "src"), 0o755); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(root, "src", "main.txt"), []byte("new"), 0o644); err != nil {
		t.Fatal(err)
	}
	local, err := localManifest(root)
	if err != nil {
		t.Fatal(err)
	}
	item, ok := local["src/main.txt"]
	if !ok || item.modTime == 0 || item.size != 3 {
		t.Fatalf("bad manifest item: %#v", item)
	}
	remote := map[string]manifestItem{
		"src/main.txt": {size: 3, modTime: item.modTime - 1},
	}
	changes := syncPlan(local, remote, "local_to_remote")
	if len(changes) != 1 || changes[0].Kind != "update" || changes[0].Path != "src/main.txt" {
		t.Fatalf("unexpected changes: %#v", changes)
	}
	remote["src/main.txt"] = item
	if changes := syncPlan(local, remote, "local_to_remote"); len(changes) != 0 {
		t.Fatalf("matching metadata should not be synced: %#v", changes)
	}
}

func TestMissingLocalManifestIsEmptyForDownloadSync(t *testing.T) {
	manifest, err := localManifest(filepath.Join(t.TempDir(), "not-created"))
	if err != nil {
		t.Fatal(err)
	}
	if len(manifest) != 0 {
		t.Fatalf("expected empty manifest, got %#v", manifest)
	}
}

func TestSecretFrameStaysOutOfJSONAndUsesWipeableBytes(t *testing.T) {
	secret := []byte("correct horse battery staple")
	jsonFrame := []byte(`{"op":"dial","auth":{"kind":"password"},"secret_len":28}` + "\n")
	if bytes.Contains(jsonFrame, secret) {
		t.Fatal("password leaked into JSON frame")
	}
	wire := append(append(append([]byte{}, jsonFrame...), secret...), '\n')
	message, err := readControlMessage(bufio.NewReader(bytes.NewReader(wire)))
	if err != nil {
		t.Fatal(err)
	}
	if message.Auth == nil || !bytes.Equal(message.Auth.Password, secret) {
		t.Fatalf("secret frame was not attached to password auth: %#v", message.Auth)
	}
	message.Auth.clear()
	if message.Auth.Password != nil {
		t.Fatal("password bytes were not released after clearing")
	}
}

func TestHostKeyRequiresExplicitApprovalBeforePersisting(t *testing.T) {
	filename := filepath.Join(t.TempDir(), "known_hosts")
	if err := os.WriteFile(filename, nil, 0o600); err != nil {
		t.Fatal(err)
	}
	pub, _, err := ed25519.GenerateKey(rand.Reader)
	if err != nil {
		t.Fatal(err)
	}
	key, err := ssh.NewPublicKey(pub)
	if err != nil {
		t.Fatal(err)
	}
	remote := &net.TCPAddr{IP: net.ParseIP("192.0.2.10"), Port: 22}

	var challenge *hostKeyChallenge
	callback, err := hostKeyCallback(filename, "", false, nil, &challenge)
	if err != nil {
		t.Fatal(err)
	}
	if err := callback("example.test:22", remote, key); err == nil {
		t.Fatal("unknown key was accepted without approval")
	}
	if challenge == nil || challenge.fingerprint != ssh.FingerprintSHA256(key) {
		t.Fatalf("missing or incorrect challenge: %#v", challenge)
	}

	callback, err = hostKeyCallback(filename, challenge.key, false, nil, &challenge)
	if err != nil {
		t.Fatal(err)
	}
	if err := callback("example.test:22", remote, key); err != nil {
		t.Fatalf("approved key was rejected: %v", err)
	}
	verifier, err := knownhosts.New(filename)
	if err != nil {
		t.Fatal(err)
	}
	if err := verifier("example.test:22", remote, key); err != nil {
		t.Fatalf("persisted key did not verify: %v", err)
	}
}

func TestChangedHostKeyRequiresExplicitReplacement(t *testing.T) {
	filename := filepath.Join(t.TempDir(), "known_hosts")
	pub1, _, _ := ed25519.GenerateKey(rand.Reader)
	pub2, _, _ := ed25519.GenerateKey(rand.Reader)
	key1, _ := ssh.NewPublicKey(pub1)
	key2, _ := ssh.NewPublicKey(pub2)
	line := knownhosts.Line([]string{"example.test"}, key1) + "\n"
	if err := os.WriteFile(filename, []byte(line), 0o600); err != nil {
		t.Fatal(err)
	}
	var challenge *hostKeyChallenge
	approvedKey := base64.StdEncoding.EncodeToString(key2.Marshal())
	callback, err := hostKeyCallback(filename, approvedKey, false, nil, &challenge)
	if err != nil {
		t.Fatal(err)
	}
	err = callback("example.test:22", &net.TCPAddr{IP: net.ParseIP("192.0.2.10"), Port: 22}, key2)
	if err == nil {
		t.Fatal("changed key was accepted without explicit replacement")
	}
	if challenge != nil {
		if !challenge.changed || len(challenge.knownFingerprints) != 1 || challenge.knownFingerprints[0] != ssh.FingerprintSHA256(key1) {
			t.Fatalf("incorrect changed-key challenge: %#v", challenge)
		}
	} else {
		t.Fatal("changed key did not produce a comparison challenge")
	}

	callback, err = hostKeyCallback(filename, approvedKey, true, challenge.knownFingerprints, &challenge)
	if err != nil {
		t.Fatal(err)
	}
	if err := callback("example.test:22", &net.TCPAddr{IP: net.ParseIP("192.0.2.10"), Port: 22}, key2); err != nil {
		t.Fatalf("explicitly approved replacement was rejected: %v", err)
	}
	verifier, err := knownhosts.New(filename)
	if err != nil {
		t.Fatal(err)
	}
	if err := verifier("example.test:22", &net.TCPAddr{IP: net.ParseIP("192.0.2.10"), Port: 22}, key2); err != nil {
		t.Fatalf("replacement key was not persisted: %v", err)
	}
}

func TestTransferPoolCapsConcurrency(t *testing.T) {
	jobs := make([]transferJob, 40)
	var active int32
	var peak int32
	err := runTransferJobs(jobs, func(transferJob) error {
		now := atomic.AddInt32(&active, 1)
		for {
			old := atomic.LoadInt32(&peak)
			if now <= old || atomic.CompareAndSwapInt32(&peak, old, now) {
				break
			}
		}
		time.Sleep(time.Millisecond)
		atomic.AddInt32(&active, -1)
		return nil
	})
	if err != nil {
		t.Fatal(err)
	}
	if peak > transferWorkers {
		t.Fatalf("peak concurrency %d exceeded cap %d", peak, transferWorkers)
	}
}
