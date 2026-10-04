// otm-agent — openterm sidecar.
// multiplexes SSH connections and serves SFTP over a local control socket.
// one agent instance per openterm instance; rust spawns it as a child.
//
// protocol (newline-delimited JSON over the control socket):
//   -> {"op":"dial","id":"uuid","host":"...","port":22,"user":"...","auth":{...}}
//   <- {"id":"uuid","ok":true,"session":"sid"}  OR  {"id":"uuid","ok":false,"err":"..."}
//   -> {"op":"exec","session":"sid","cmd":"ls -la","pty":true,"cols":100,"rows":30}
//   <- {"session":"sid","chan":"cid","ok":true}
//   -> {"op":"write","chan":"cid","data":"base64..."}
//   <- {"chan":"cid","stream":"stdout","data":"base64..."}
//   -> {"op":"resize","chan":"cid","cols":120,"rows":40}
//   -> {"op":"sftp_ls","session":"sid","path":"/home"} -> {"entries":[...]}
//   -> {"op":"sftp_read","session":"sid","path":"/home/x"} -> {"data":"base64..."}
//   -> {"op":"device_info","session":"sid"} -> {"hostname":"...","platform":"Linux"}
//   -> {"op":"sftp_get","session":"sid","remote":"/x","local":"/y"}
//   -> {"op":"close","chan":"cid"}
//   -> {"op":"disconnect","session":"sid"}
//
// why go: crypto/ssh is rock-solid, sftp support is clean, static binary ships
// with no runtime deps. we get parallel SSH session mux "for free" via goroutines.

package main

import (
	"bufio"
	"bytes"
	"encoding/base64"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"log"
	"net"
	"os"
	"path"
	"path/filepath"
	"runtime"
	"sort"
	"strings"
	"sync"
	"time"

	"github.com/pkg/sftp"
	"golang.org/x/crypto/ssh"
	"golang.org/x/crypto/ssh/agent"
	"golang.org/x/crypto/ssh/knownhosts"
)

type Msg struct {
	Op                string          `json:"op,omitempty"`
	ID                string          `json:"id,omitempty"`
	Session           string          `json:"session,omitempty"`
	Chan              string          `json:"chan,omitempty"`
	Stream            string          `json:"stream,omitempty"`
	Host              string          `json:"host,omitempty"`
	Port              int             `json:"port,omitempty"`
	User              string          `json:"user,omitempty"`
	Auth              *AuthSpec       `json:"auth,omitempty"`
	Cmd               string          `json:"cmd,omitempty"`
	Pty               bool            `json:"pty,omitempty"`
	Cols              int             `json:"cols,omitempty"`
	Rows              int             `json:"rows,omitempty"`
	Path              string          `json:"path,omitempty"`
	Remote            string          `json:"remote,omitempty"`
	Local             string          `json:"local,omitempty"`
	Data              string          `json:"data,omitempty"`
	Ok                bool            `json:"ok,omitempty"`
	Err               string          `json:"err,omitempty"`
	Message           string          `json:"message,omitempty"`
	HostKey           string          `json:"host_key,omitempty"`
	Fingerprint       string          `json:"fingerprint,omitempty"`
	KnownHosts        string          `json:"known_hosts,omitempty"`
	KnownFingerprints []string        `json:"known_fingerprints,omitempty"`
	TrustHostKey      string          `json:"trust_host_key,omitempty"`
	ReplaceHostKey    bool            `json:"replace_host_key,omitempty"`
	SecretLen         int             `json:"secret_len,omitempty"`
	Protocol          int             `json:"protocol,omitempty"`
	Entries           []FileEntry     `json:"entries,omitempty"`
	Hostname          string          `json:"hostname,omitempty"`
	Platform          string          `json:"platform,omitempty"`
	Direction         string          `json:"direction,omitempty"`
	Changes           []SyncChange    `json:"changes,omitempty"`
	Raw               json.RawMessage `json:"-"`
}

type AuthSpec struct {
	Kind       string `json:"kind"` // password | key | agent
	KeyPath    string `json:"key_path,omitempty"`
	Password   []byte `json:"-"`
	Passphrase []byte `json:"-"`
}

func (auth *AuthSpec) clear() {
	if auth == nil {
		return
	}
	clear(auth.Password)
	clear(auth.Passphrase)
	auth.Password = nil
	auth.Passphrase = nil
}

type FileEntry struct {
	Name string `json:"name"`
	Size int64  `json:"size"`
	Mode uint32 `json:"mode"`
	Dir  bool   `json:"dir"`
	Mtim int64  `json:"mtim"`
}

type SyncChange struct {
	Path string `json:"path"`
	Kind string `json:"kind"`
	Size int64  `json:"size"`
}

type Session struct {
	id        string
	client    *ssh.Client
	sftp      *sftp.Client
	agentConn io.Closer
	platform  string
	mu        sync.Mutex
}

type Channel struct {
	id      string
	session *ssh.Session
	stdin   io.WriteCloser
}

type Agent struct {
	mu       sync.Mutex
	sess     map[string]*Session
	channels map[string]channelEntry
	out      *json.Encoder
	outMu    sync.Mutex
}

type channelEntry struct {
	sessionID string
	channel   *Channel
}

type hostKeyChallenge struct {
	key               string
	fingerprint       string
	knownHosts        string
	knownFingerprints []string
	changed           bool
}

type unknownHostKeyError struct{}

func (unknownHostKeyError) Error() string { return "unknown SSH host key" }

func (a *Agent) send(m Msg) {
	a.outMu.Lock()
	defer a.outMu.Unlock()
	_ = a.out.Encode(&m)
}

func (a *Agent) handle(m Msg) {
	switch m.Op {
	case "hello":
		a.send(Msg{ID: m.ID, Ok: true, Protocol: agentProtocolVersion})
	case "dial":
		a.dial(m)
	case "exec":
		a.exec(m)
	case "write":
		a.write(m)
	case "resize":
		a.resize(m)
	case "sftp_ls":
		a.sftpLs(m)
	case "sftp_read":
		a.sftpRead(m)
	case "sftp_write":
		a.sftpWrite(m)
	case "device_info":
		a.deviceInfo(m)
	case "sftp_get":
		a.sftpGet(m)
	case "sftp_put":
		a.sftpPut(m)
	case "sftp_get_tree":
		a.sftpGetTree(m)
	case "sftp_put_tree":
		a.sftpPutTree(m)
	case "sftp_sync_plan":
		a.sftpSyncPlan(m)
	case "sftp_sync_apply":
		a.sftpSyncApply(m)
	case "close":
		a.closeChan(m)
	case "disconnect":
		a.disconnect(m)
	default:
		a.send(Msg{ID: m.ID, Ok: false, Err: "unknown op: " + m.Op})
	}
}

func knownHostsPath() (string, error) {
	home, err := os.UserHomeDir()
	if err != nil {
		return "", fmt.Errorf("find home directory: %w", err)
	}
	dir := filepath.Join(home, ".ssh")
	if err := os.MkdirAll(dir, 0o700); err != nil {
		return "", fmt.Errorf("create .ssh directory: %w", err)
	}
	filename := filepath.Join(dir, "known_hosts")
	f, err := os.OpenFile(filename, os.O_CREATE|os.O_APPEND, 0o600)
	if err != nil {
		return "", fmt.Errorf("open known_hosts: %w", err)
	}
	if err := f.Close(); err != nil {
		return "", fmt.Errorf("close known_hosts: %w", err)
	}
	return filename, nil
}

var knownHostsMu sync.Mutex

func sameStrings(left, right []string) bool {
	if len(left) != len(right) {
		return false
	}
	left = append([]string(nil), left...)
	right = append([]string(nil), right...)
	sort.Strings(left)
	sort.Strings(right)
	for i := range left {
		if left[i] != right[i] {
			return false
		}
	}
	return true
}

func replaceKnownHostKeys(filename, hostname string, oldKeys []knownhosts.KnownKey, newKey ssh.PublicKey) error {
	knownHostsMu.Lock()
	defer knownHostsMu.Unlock()

	contents, err := os.ReadFile(filename)
	if err != nil {
		return err
	}
	lines := strings.Split(strings.ReplaceAll(string(contents), "\r\n", "\n"), "\n")
	remove := make(map[int]string, len(oldKeys))
	for _, old := range oldKeys {
		if filepath.Clean(old.Filename) != filepath.Clean(filename) {
			return fmt.Errorf("refusing to replace a host key from another file")
		}
		remove[old.Line] = base64.StdEncoding.EncodeToString(old.Key.Marshal())
	}
	for lineNumber, encodedKey := range remove {
		if lineNumber < 1 || lineNumber > len(lines) {
			return fmt.Errorf("known_hosts changed while confirming the replacement")
		}
		found := false
		for _, field := range strings.Fields(lines[lineNumber-1]) {
			if field == encodedKey {
				found = true
				break
			}
		}
		if !found {
			return fmt.Errorf("known_hosts changed while confirming the replacement")
		}
	}

	updated := make([]string, 0, len(lines)+1)
	for i, line := range lines {
		if _, shouldRemove := remove[i+1]; shouldRemove || line == "" {
			continue
		}
		updated = append(updated, line)
	}
	updated = append(updated, knownhosts.Line([]string{knownhosts.Normalize(hostname)}, newKey))
	return os.WriteFile(filename, []byte(strings.Join(updated, "\n")+"\n"), 0o600)
}

func hostKeyCallback(filename, approvedKey string, replace bool, expectedFingerprints []string, challenge **hostKeyChallenge) (ssh.HostKeyCallback, error) {
	knownHostsMu.Lock()
	verifier, err := knownhosts.New(filename)
	knownHostsMu.Unlock()
	if err != nil {
		return nil, err
	}
	return func(hostname string, remote net.Addr, key ssh.PublicKey) error {
		err := verifier(hostname, remote, key)
		if err == nil {
			return nil
		}
		var keyErr *knownhosts.KeyError
		if !errors.As(err, &keyErr) {
			return err
		}
		encoded := base64.StdEncoding.EncodeToString(key.Marshal())
		knownFingerprints := make([]string, 0, len(keyErr.Want))
		for _, known := range keyErr.Want {
			knownFingerprints = append(knownFingerprints, ssh.FingerprintSHA256(known.Key))
		}
		if len(keyErr.Want) != 0 {
			if approvedKey == encoded && replace && sameStrings(knownFingerprints, expectedFingerprints) {
				return replaceKnownHostKeys(filename, hostname, keyErr.Want, key)
			}
			*challenge = &hostKeyChallenge{
				key:               encoded,
				fingerprint:       ssh.FingerprintSHA256(key),
				knownHosts:        filename,
				knownFingerprints: knownFingerprints,
				changed:           true,
			}
			return unknownHostKeyError{}
		}
		if approvedKey == encoded {
			line := knownhosts.Line([]string{knownhosts.Normalize(hostname)}, key)
			knownHostsMu.Lock()
			defer knownHostsMu.Unlock()
			f, openErr := os.OpenFile(filename, os.O_APPEND|os.O_WRONLY, 0o600)
			if openErr != nil {
				return openErr
			}
			if _, writeErr := fmt.Fprintln(f, line); writeErr != nil {
				_ = f.Close()
				return writeErr
			}
			return f.Close()
		}
		*challenge = &hostKeyChallenge{
			key:               encoded,
			fingerprint:       ssh.FingerprintSHA256(key),
			knownHosts:        filename,
			knownFingerprints: nil,
			changed:           false,
		}
		return unknownHostKeyError{}
	}, nil
}

func dialSSHAgent() (io.ReadWriteCloser, error) {
	sock := os.Getenv("SSH_AUTH_SOCK")
	if sock != "" {
		if strings.HasPrefix(strings.ToLower(sock), `\\.\pipe\`) {
			return os.OpenFile(sock, os.O_RDWR, 0)
		}
		return net.Dial("unix", sock)
	}
	if runtime.GOOS == "windows" {
		return os.OpenFile(`\\.\pipe\openssh-ssh-agent`, os.O_RDWR, 0)
	}
	return nil, fmt.Errorf("no SSH_AUTH_SOCK")
}

func (a *Agent) dial(m Msg) {
	knownHostsFile, err := knownHostsPath()
	if err != nil {
		a.send(Msg{ID: m.ID, Ok: false, Err: err.Error()})
		return
	}
	var challenge *hostKeyChallenge
	callback, err := hostKeyCallback(knownHostsFile, m.TrustHostKey, m.ReplaceHostKey, m.KnownFingerprints, &challenge)
	if err != nil {
		a.send(Msg{ID: m.ID, Ok: false, Err: err.Error()})
		return
	}
	cfg := &ssh.ClientConfig{
		User:            m.User,
		HostKeyCallback: callback,
		Timeout:         10 * time.Second,
	}
	if m.Auth == nil {
		a.send(Msg{ID: m.ID, Ok: false, Err: "no auth"})
		return
	}
	auth := m.Auth
	defer auth.clear()
	var agentConn io.ReadWriteCloser
	switch m.Auth.Kind {
	case "password":
		// x/crypto/ssh's public password API requires a string. Create that
		// unavoidable immutable copy only inside the authentication callback,
		// rather than retaining it for the entire dial attempt.
		cfg.Auth = []ssh.AuthMethod{ssh.PasswordCallback(func() (string, error) {
			return string(auth.Password), nil
		})}
	case "key":
		buf, err := os.ReadFile(m.Auth.KeyPath)
		if err != nil {
			a.send(Msg{ID: m.ID, Ok: false, Err: err.Error()})
			return
		}
		defer clear(buf)
		var signer ssh.Signer
		if len(auth.Passphrase) != 0 {
			signer, err = ssh.ParsePrivateKeyWithPassphrase(buf, auth.Passphrase)
		} else {
			signer, err = ssh.ParsePrivateKey(buf)
		}
		if err != nil {
			a.send(Msg{ID: m.ID, Ok: false, Err: err.Error()})
			return
		}
		cfg.Auth = []ssh.AuthMethod{ssh.PublicKeys(signer)}
	case "agent":
		agentConn, err = dialSSHAgent()
		if err != nil {
			a.send(Msg{ID: m.ID, Ok: false, Err: err.Error()})
			return
		}
		cfg.Auth = []ssh.AuthMethod{ssh.PublicKeysCallback(agent.NewClient(agentConn).Signers)}
	default:
		a.send(Msg{ID: m.ID, Ok: false, Err: "bad auth kind"})
		return
	}

	addr := fmt.Sprintf("%s:%d", m.Host, m.Port)
	cli, err := ssh.Dial("tcp", addr, cfg)
	cfg.Auth = nil
	auth.clear()
	m.Auth = nil
	if err != nil {
		if agentConn != nil {
			_ = agentConn.Close()
		}
		if challenge != nil {
			a.send(Msg{Op: "host_key", ID: m.ID, Ok: false, HostKey: challenge.key, Fingerprint: challenge.fingerprint, KnownHosts: challenge.knownHosts, KnownFingerprints: challenge.knownFingerprints, ReplaceHostKey: challenge.changed})
			return
		}
		a.send(Msg{ID: m.ID, Ok: false, Err: err.Error()})
		return
	}
	sid := m.ID
	sess := &Session{id: sid, client: cli, agentConn: agentConn}
	a.mu.Lock()
	a.sess[sid] = sess
	a.mu.Unlock()
	a.send(Msg{ID: m.ID, Ok: true, Session: sid})
}

func (a *Agent) exec(m Msg) {
	s := a.getSess(m.Session)
	if s == nil {
		a.send(Msg{Chan: m.Chan, Ok: false, Err: "no such session"})
		return
	}
	sess, err := s.client.NewSession()
	if err != nil {
		a.send(Msg{Session: m.Session, Ok: false, Err: err.Error()})
		return
	}
	if m.Pty {
		modes := ssh.TerminalModes{ssh.ECHO: 1, ssh.TTY_OP_ISPEED: 14400, ssh.TTY_OP_OSPEED: 14400}
		if err := sess.RequestPty("xterm-256color", m.Rows, m.Cols, modes); err != nil {
			_ = sess.Close()
			a.send(Msg{Session: m.Session, Ok: false, Err: err.Error()})
			return
		}
	}
	stdin, err := sess.StdinPipe()
	if err != nil {
		_ = sess.Close()
		a.send(Msg{Chan: m.Chan, Ok: false, Err: "open stdin: " + err.Error()})
		return
	}
	stdout, err := sess.StdoutPipe()
	if err != nil {
		_ = sess.Close()
		a.send(Msg{Chan: m.Chan, Ok: false, Err: "open stdout: " + err.Error()})
		return
	}
	stderr, err := sess.StderrPipe()
	if err != nil {
		_ = sess.Close()
		a.send(Msg{Chan: m.Chan, Ok: false, Err: "open stderr: " + err.Error()})
		return
	}

	cid := m.Chan
	if cid == "" {
		cid = fmt.Sprintf("c%d", time.Now().UnixNano())
	}
	ch := &Channel{id: cid, session: sess, stdin: stdin}

	if m.Cmd == "" {
		if err := sess.Shell(); err != nil {
			_ = sess.Close()
			a.send(Msg{Chan: cid, Ok: false, Err: err.Error()})
			return
		}
	} else {
		if err := sess.Start(m.Cmd); err != nil {
			_ = sess.Close()
			a.send(Msg{Chan: cid, Ok: false, Err: err.Error()})
			return
		}
	}
	a.mu.Lock()
	a.channels[cid] = channelEntry{sessionID: m.Session, channel: ch}
	a.mu.Unlock()
	a.send(Msg{Session: m.Session, Chan: cid, Ok: true})

	go a.pump(cid, "stdout", stdout)
	go a.pump(cid, "stderr", stderr)
	go func() {
		_ = sess.Wait()
		a.removeChannel(cid)
		a.send(Msg{Chan: cid, Stream: "exit", Ok: true})
	}()
}

func (a *Agent) pump(cid, stream string, r io.Reader) {
	buf := make([]byte, 64*1024)
	for {
		n, err := r.Read(buf)
		if n > 0 {
			a.send(Msg{Chan: cid, Stream: stream, Data: base64.StdEncoding.EncodeToString(buf[:n])})
		}
		if err != nil {
			return
		}
	}
}

func (a *Agent) write(m Msg) {
	ch := a.findChan(m.Chan)
	if ch == nil {
		return
	}
	data, err := base64.StdEncoding.DecodeString(m.Data)
	if err != nil {
		return
	}
	_, _ = ch.stdin.Write(data)
}

func (a *Agent) resize(m Msg) {
	ch := a.findChan(m.Chan)
	if ch == nil {
		return
	}
	_ = ch.session.WindowChange(m.Rows, m.Cols)
}

func (a *Agent) closeChan(m Msg) {
	ch := a.findChan(m.Chan)
	if ch != nil {
		_ = ch.session.Close()
	}
	a.removeChannel(m.Chan)
}

func (a *Agent) disconnect(m Msg) {
	a.mu.Lock()
	s, ok := a.sess[m.Session]
	delete(a.sess, m.Session)
	for cid, entry := range a.channels {
		if entry.sessionID == m.Session {
			delete(a.channels, cid)
		}
	}
	a.mu.Unlock()
	if !ok {
		return
	}
	if s.sftp != nil {
		_ = s.sftp.Close()
	}
	_ = s.client.Close()
	if s.agentConn != nil {
		_ = s.agentConn.Close()
	}
	a.send(Msg{Session: m.Session, Ok: true})
}

func (a *Agent) ensureSftp(s *Session) error {
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.sftp != nil {
		return nil
	}
	c, err := sftp.NewClient(s.client)
	if err != nil {
		return err
	}
	s.sftp = c
	return nil
}

func (a *Agent) sftpLs(m Msg) {
	s := a.getSess(m.Session)
	if s == nil {
		a.send(Msg{ID: m.ID, Ok: false, Err: "no session"})
		return
	}
	if err := a.ensureSftp(s); err != nil {
		a.send(Msg{ID: m.ID, Ok: false, Err: err.Error()})
		return
	}
	requested := m.Path
	if requested == "" {
		requested = "."
	}
	// Ask the server for its canonical path. This avoids making assumptions
	// about the remote host from the client OS (notably Windows clients viewing
	// Unix servers, and vice versa).
	resolved, err := s.sftp.RealPath(requested)
	if err != nil {
		resolved = requested
	}
	fis, err := s.sftp.ReadDir(resolved)
	if err != nil {
		a.send(Msg{ID: m.ID, Ok: false, Err: err.Error()})
		return
	}
	out := make([]FileEntry, 0, len(fis))
	for _, fi := range fis {
		out = append(out, FileEntry{
			Name: fi.Name(), Size: fi.Size(), Mode: uint32(fi.Mode()),
			Dir: fi.IsDir(), Mtim: fi.ModTime().Unix(),
		})
	}
	a.send(Msg{ID: m.ID, Ok: true, Path: resolved, Entries: out})
}

const maxEditorFileSize = 4 * 1024 * 1024

func (a *Agent) sftpRead(m Msg) {
	s := a.getSess(m.Session)
	if s == nil {
		a.send(Msg{ID: m.ID, Ok: false, Err: "no session"})
		return
	}
	if err := a.ensureSftp(s); err != nil {
		a.send(Msg{ID: m.ID, Ok: false, Err: err.Error()})
		return
	}
	f, err := s.sftp.Open(m.Path)
	if err != nil {
		a.send(Msg{ID: m.ID, Ok: false, Err: err.Error()})
		return
	}
	defer f.Close()
	data, err := io.ReadAll(io.LimitReader(f, maxEditorFileSize+1))
	if err != nil {
		a.send(Msg{ID: m.ID, Ok: false, Err: err.Error()})
		return
	}
	if len(data) > maxEditorFileSize {
		a.send(Msg{ID: m.ID, Ok: false, Err: "file is over 4 MB"})
		return
	}
	a.send(Msg{ID: m.ID, Ok: true, Data: base64.StdEncoding.EncodeToString(data)})
}

func (a *Agent) sftpWrite(m Msg) {
	s := a.getSess(m.Session)
	if s == nil || a.ensureSftp(s) != nil {
		a.send(Msg{ID: m.ID, Ok: false, Err: "sftp unavailable"})
		return
	}
	data, err := base64.StdEncoding.DecodeString(m.Data)
	if err != nil {
		a.send(Msg{ID: m.ID, Ok: false, Err: "invalid file data: " + err.Error()})
		return
	}
	tmp := fmt.Sprintf("%s.openterm-%d.tmp", m.Path, time.Now().UnixNano())
	f, err := s.sftp.OpenFile(tmp, os.O_WRONLY|os.O_CREATE|os.O_TRUNC)
	if err == nil {
		var written int
		written, err = f.Write(data)
		if err == nil && written != len(data) {
			err = io.ErrShortWrite
		}
		if closeErr := f.Close(); err == nil {
			err = closeErr
		}
	}
	if err == nil {
		err = s.sftp.PosixRename(tmp, m.Path)
		if err != nil {
			// Servers without posix-rename cannot replace atomically. Keep the
			// unavoidable target-missing window short and retry transient failures
			// before restoring the original file.
			backup := fmt.Sprintf("%s.openterm-%d.bak", m.Path, time.Now().UnixNano())
			hadOld := s.sftp.Rename(m.Path, backup) == nil
			for attempt := 0; attempt < 3; attempt++ {
				err = s.sftp.Rename(tmp, m.Path)
				if err == nil {
					break
				}
				time.Sleep(time.Duration(attempt+1) * 10 * time.Millisecond)
			}
			if err != nil && hadOld {
				_ = s.sftp.Rename(backup, m.Path)
			} else if hadOld {
				_ = s.sftp.Remove(backup)
			}
		}
	}
	if err != nil {
		_ = s.sftp.Remove(tmp)
		a.send(Msg{ID: m.ID, Ok: false, Err: err.Error()})
		return
	}
	a.send(Msg{ID: m.ID, Ok: true})
}

func remoteOutput(client *ssh.Client, command string) (string, error) {
	session, err := client.NewSession()
	if err != nil {
		return "", err
	}
	defer session.Close()
	out, err := session.Output(command)
	return strings.TrimSpace(string(out)), err
}

func (a *Agent) deviceInfo(m Msg) {
	s := a.getSess(m.Session)
	if s == nil {
		a.send(Msg{ID: m.ID, Ok: false, Err: "no session"})
		return
	}
	hostname, hostErr := remoteOutput(s.client, "hostname")
	s.mu.Lock()
	platform := s.platform
	s.mu.Unlock()
	if platform == "" {
		if version, err := remoteOutput(s.client, "cmd.exe /d /c ver"); err == nil && strings.Contains(strings.ToLower(version), "windows") {
			platform = "Windows"
		} else if uname, err := remoteOutput(s.client, "uname -s"); err == nil && uname != "" {
			platform = uname
		} else {
			platform = "Unknown"
		}
		s.mu.Lock()
		s.platform = platform
		s.mu.Unlock()
	}
	if hostErr != nil || hostname == "" {
		hostname = "unknown host"
	}
	a.send(Msg{ID: m.ID, Ok: true, Hostname: hostname, Platform: platform})
}

func (a *Agent) sftpGet(m Msg) {
	s := a.getSess(m.Session)
	if s == nil || a.ensureSftp(s) != nil {
		a.send(Msg{ID: m.ID, Ok: false, Err: "sftp unavailable"})
		return
	}
	info, err := s.sftp.Stat(m.Remote)
	if err != nil {
		a.send(Msg{ID: m.ID, Ok: false, Err: err.Error()})
		return
	}
	if err := copyRemoteToLocal(s.sftp, m.Remote, m.Local); err != nil {
		a.send(Msg{ID: m.ID, Ok: false, Err: err.Error()})
		return
	}
	a.send(Msg{ID: m.ID, Ok: true, Message: fmt.Sprintf("%d bytes", info.Size())})
}

func (a *Agent) sftpPut(m Msg) {
	s := a.getSess(m.Session)
	if s == nil || a.ensureSftp(s) != nil {
		a.send(Msg{ID: m.ID, Ok: false, Err: "sftp unavailable"})
		return
	}
	info, err := os.Stat(m.Local)
	if err != nil {
		a.send(Msg{ID: m.ID, Ok: false, Err: err.Error()})
		return
	}
	if err := copyLocalToRemote(s.sftp, m.Local, m.Remote); err != nil {
		a.send(Msg{ID: m.ID, Ok: false, Err: err.Error()})
		return
	}
	a.send(Msg{ID: m.ID, Ok: true, Message: fmt.Sprintf("%d bytes", info.Size())})
}

func cleanRemoteJoin(root, rel string) string {
	rel = strings.TrimLeft(filepath.ToSlash(rel), "/")
	if rel == "" || rel == "." {
		return root
	}
	return path.Join(root, rel)
}

func remoteRelative(root, name string) string {
	r := strings.TrimRight(filepath.ToSlash(root), "/")
	n := filepath.ToSlash(name)
	if n == r {
		return ""
	}
	return strings.TrimLeft(strings.TrimPrefix(n, r), "/")
}

const transferWorkers = 6

type transferJob struct {
	local  string
	remote string
	size   int64
	dir    bool
}

func runTransferJobs(jobs []transferJob, work func(transferJob) error) error {
	queue := make(chan transferJob)
	errCh := make(chan error, 1)
	var wg sync.WaitGroup
	workerCount := min(transferWorkers, len(jobs))
	for range workerCount {
		wg.Add(1)
		go func() {
			defer wg.Done()
			for job := range queue {
				if err := work(job); err != nil {
					select {
					case errCh <- err:
					default:
					}
				}
			}
		}()
	}
	for _, job := range jobs {
		queue <- job
	}
	close(queue)
	wg.Wait()
	select {
	case err := <-errCh:
		return err
	default:
		return nil
	}
}

func (a *Agent) sftpGetTree(m Msg) {
	s := a.getSess(m.Session)
	if s == nil || a.ensureSftp(s) != nil {
		a.send(Msg{ID: m.ID, Ok: false, Err: "sftp unavailable"})
		return
	}
	jobs := make([]transferJob, 0)
	walker := s.sftp.Walk(m.Remote)
	for walker.Step() {
		if walker.Err() != nil {
			a.send(Msg{ID: m.ID, Ok: false, Err: walker.Err().Error()})
			return
		}
		rel := remoteRelative(m.Remote, walker.Path())
		local := filepath.Join(m.Local, filepath.FromSlash(rel))
		if walker.Stat().IsDir() {
			if err := os.MkdirAll(local, 0o755); err != nil {
				a.send(Msg{ID: m.ID, Ok: false, Err: err.Error()})
				return
			}
			continue
		}
		if !walker.Stat().Mode().IsRegular() {
			continue
		}
		jobs = append(jobs, transferJob{remote: walker.Path(), local: local, size: walker.Stat().Size()})
	}
	if err := runTransferJobs(jobs, func(job transferJob) error {
		return copyRemoteToLocal(s.sftp, job.remote, job.local)
	}); err != nil {
		a.send(Msg{ID: m.ID, Ok: false, Err: err.Error()})
		return
	}
	total := int64(0)
	for _, job := range jobs {
		total += job.size
	}
	a.send(Msg{ID: m.ID, Ok: true, Message: fmt.Sprintf("downloaded %d files (%d bytes)", len(jobs), total)})
}

func (a *Agent) sftpPutTree(m Msg) {
	s := a.getSess(m.Session)
	if s == nil || a.ensureSftp(s) != nil {
		a.send(Msg{ID: m.ID, Ok: false, Err: "sftp unavailable"})
		return
	}
	jobs := make([]transferJob, 0)
	err := filepath.Walk(m.Local, func(name string, info os.FileInfo, walkErr error) error {
		if walkErr != nil {
			if os.IsPermission(walkErr) && filepath.Clean(name) != filepath.Clean(m.Local) {
				if info != nil && info.IsDir() {
					return filepath.SkipDir
				}
				return nil
			}
			return walkErr
		}
		rel, err := filepath.Rel(m.Local, name)
		if err != nil {
			return err
		}
		remote := cleanRemoteJoin(m.Remote, rel)
		if info.IsDir() {
			jobs = append(jobs, transferJob{remote: remote, dir: true})
			return nil
		}
		if !info.Mode().IsRegular() {
			return nil
		}
		jobs = append(jobs, transferJob{local: name, remote: remote, size: info.Size()})
		return nil
	})
	if err != nil {
		a.send(Msg{ID: m.ID, Ok: false, Err: err.Error()})
		return
	}
	if err := runTransferJobs(jobs, func(job transferJob) error {
		if job.dir {
			return s.sftp.MkdirAll(job.remote)
		}
		return copyLocalToRemote(s.sftp, job.local, job.remote)
	}); err != nil {
		a.send(Msg{ID: m.ID, Ok: false, Err: err.Error()})
		return
	}
	count, total := 0, int64(0)
	for _, job := range jobs {
		if !job.dir {
			count++
			total += job.size
		}
	}
	a.send(Msg{ID: m.ID, Ok: true, Message: fmt.Sprintf("uploaded %d files (%d bytes)", count, total)})
}

type manifestItem struct {
	size    int64
	modTime int64
}

func localManifest(root string) (map[string]manifestItem, error) {
	out := map[string]manifestItem{}
	err := filepath.Walk(root, func(name string, info os.FileInfo, walkErr error) error {
		if walkErr != nil {
			if os.IsPermission(walkErr) && filepath.Clean(name) != filepath.Clean(root) {
				if info != nil && info.IsDir() {
					return filepath.SkipDir
				}
				return nil
			}
			return walkErr
		}
		if info.IsDir() || !info.Mode().IsRegular() {
			return nil
		}
		rel, err := filepath.Rel(root, name)
		if err != nil {
			return err
		}
		out[filepath.ToSlash(rel)] = manifestItem{
			size:    info.Size(),
			modTime: info.ModTime().Unix(),
		}
		return nil
	})
	if os.IsNotExist(err) {
		return out, nil
	}
	return out, err
}

func remoteManifest(client *sftp.Client, root string) (map[string]manifestItem, error) {
	out := map[string]manifestItem{}
	walker := client.Walk(root)
	for walker.Step() {
		if walker.Err() != nil {
			return nil, walker.Err()
		}
		info := walker.Stat()
		if info.IsDir() || !info.Mode().IsRegular() {
			continue
		}
		out[remoteRelative(root, walker.Path())] = manifestItem{
			size:    info.Size(),
			modTime: info.ModTime().Unix(),
		}
	}
	return out, nil
}

func syncPlan(local, remote map[string]manifestItem, direction string) []SyncChange {
	source, dest := local, remote
	if direction == "remote_to_local" {
		source, dest = remote, local
	}
	changes := make([]SyncChange, 0)
	for name, src := range source {
		dst, exists := dest[name]
		kind := "add"
		if exists {
			if dst.size == src.size && dst.modTime == src.modTime {
				continue
			}
			kind = "update"
		}
		changes = append(changes, SyncChange{Path: name, Kind: kind, Size: src.size})
	}
	sort.Slice(changes, func(i, j int) bool { return changes[i].Path < changes[j].Path })
	return changes
}

func (a *Agent) syncState(m Msg) (*Session, []SyncChange, error) {
	s := a.getSess(m.Session)
	if s == nil {
		return nil, nil, fmt.Errorf("no session")
	}
	if err := a.ensureSftp(s); err != nil {
		return nil, nil, err
	}
	localInfo, localStatErr := os.Stat(m.Local)
	if localStatErr != nil && !(m.Direction == "remote_to_local" && os.IsNotExist(localStatErr)) {
		return nil, nil, localStatErr
	}
	if localStatErr == nil && !localInfo.IsDir() {
		return nil, nil, fmt.Errorf("local sync path is not a directory")
	}
	remoteInfo, remoteStatErr := s.sftp.Stat(m.Remote)
	if remoteStatErr != nil && !(m.Direction == "local_to_remote" && os.IsNotExist(remoteStatErr)) {
		return nil, nil, remoteStatErr
	}
	if remoteStatErr == nil && !remoteInfo.IsDir() {
		return nil, nil, fmt.Errorf("remote sync path is not a directory")
	}
	local, err := localManifest(m.Local)
	if err != nil {
		return nil, nil, err
	}
	remote := map[string]manifestItem{}
	if remoteStatErr == nil {
		remote, err = remoteManifest(s.sftp, m.Remote)
		if err != nil {
			return nil, nil, err
		}
	}
	return s, syncPlan(local, remote, m.Direction), nil
}

func (a *Agent) sftpSyncPlan(m Msg) {
	_, changes, err := a.syncState(m)
	if err != nil {
		a.send(Msg{ID: m.ID, Ok: false, Err: err.Error()})
		return
	}
	a.send(Msg{ID: m.ID, Ok: true, Changes: changes})
}

func (a *Agent) sftpSyncApply(m Msg) {
	s, changes, err := a.syncState(m)
	if err != nil {
		a.send(Msg{ID: m.ID, Ok: false, Err: err.Error()})
		return
	}
	jobs := make([]transferJob, 0, len(changes))
	for _, change := range changes {
		jobs = append(jobs, transferJob{
			local:  filepath.Join(m.Local, filepath.FromSlash(change.Path)),
			remote: cleanRemoteJoin(m.Remote, change.Path),
			size:   change.Size,
		})
	}
	err = runTransferJobs(jobs, func(job transferJob) error {
		if m.Direction == "remote_to_local" {
			return copyRemoteToLocal(s.sftp, job.remote, job.local)
		}
		return copyLocalToRemote(s.sftp, job.local, job.remote)
	})
	if err != nil {
		a.send(Msg{ID: m.ID, Ok: false, Err: err.Error()})
		return
	}
	a.send(Msg{ID: m.ID, Ok: true, Message: fmt.Sprintf("synced %d files", len(changes))})
}

func copyRemoteToLocal(client *sftp.Client, remote, local string) error {
	info, err := client.Stat(remote)
	if err != nil {
		return err
	}
	src, err := client.Open(remote)
	if err != nil {
		return err
	}
	defer src.Close()
	if err := os.MkdirAll(filepath.Dir(local), 0o755); err != nil {
		return err
	}
	dst, err := os.Create(local)
	if err != nil {
		return err
	}
	_, copyErr := io.Copy(dst, src)
	closeErr := dst.Close()
	if copyErr != nil {
		return copyErr
	}
	if closeErr != nil {
		return closeErr
	}
	// Timestamp preservation makes later metadata previews instant. Treat it as
	// best-effort because a few SFTP servers/filesystems forbid setting times.
	_ = os.Chtimes(local, info.ModTime(), info.ModTime())
	return nil
}

func copyLocalToRemote(client *sftp.Client, local, remote string) error {
	info, err := os.Stat(local)
	if err != nil {
		return err
	}
	src, err := os.Open(local)
	if err != nil {
		return err
	}
	defer src.Close()
	if err := client.MkdirAll(path.Dir(remote)); err != nil {
		return err
	}
	dst, err := client.Create(remote)
	if err != nil {
		return err
	}
	_, copyErr := io.Copy(dst, src)
	closeErr := dst.Close()
	if copyErr != nil {
		return copyErr
	}
	if closeErr != nil {
		return closeErr
	}
	_ = client.Chtimes(remote, info.ModTime(), info.ModTime())
	return nil
}

func (a *Agent) getSess(id string) *Session {
	a.mu.Lock()
	defer a.mu.Unlock()
	return a.sess[id]
}

func (a *Agent) findChan(cid string) *Channel {
	a.mu.Lock()
	defer a.mu.Unlock()
	if entry, ok := a.channels[cid]; ok {
		return entry.channel
	}
	return nil
}

func (a *Agent) removeChannel(cid string) {
	a.mu.Lock()
	defer a.mu.Unlock()
	delete(a.channels, cid)
}

const (
	agentProtocolVersion = 3
	maxControlFrame      = 16 * 1024 * 1024
	maxSecretSize        = 1024 * 1024
)

func readControlLine(reader *bufio.Reader) ([]byte, error) {
	line := make([]byte, 0, 4096)
	for {
		fragment, err := reader.ReadSlice('\n')
		if len(line)+len(fragment) > maxControlFrame {
			clear(line)
			return nil, fmt.Errorf("control frame exceeds %d bytes", maxControlFrame)
		}
		line = append(line, fragment...)
		if err == bufio.ErrBufferFull {
			continue
		}
		if err != nil {
			clear(line)
			return nil, err
		}
		return bytes.TrimSpace(line), nil
	}
}

func readControlMessage(reader *bufio.Reader) (Msg, error) {
	var message Msg
	line, err := readControlLine(reader)
	if err != nil {
		return message, err
	}
	defer clear(line)
	if err := json.Unmarshal(line, &message); err != nil {
		return message, fmt.Errorf("parse: %w", err)
	}
	if message.SecretLen < 0 || message.SecretLen > maxSecretSize {
		return message, fmt.Errorf("invalid secret length %d", message.SecretLen)
	}
	if message.SecretLen == 0 {
		return message, nil
	}
	secret := make([]byte, message.SecretLen)
	if _, err := io.ReadFull(reader, secret); err != nil {
		clear(secret)
		return message, fmt.Errorf("read secret frame: %w", err)
	}
	terminator, err := reader.ReadByte()
	if err != nil || terminator != '\n' {
		clear(secret)
		return message, fmt.Errorf("invalid secret frame terminator")
	}
	if message.Auth == nil {
		clear(secret)
		return message, fmt.Errorf("secret frame without authentication")
	}
	switch message.Auth.Kind {
	case "password":
		message.Auth.Password = secret
	case "key":
		message.Auth.Passphrase = secret
	default:
		clear(secret)
		return message, fmt.Errorf("unexpected secret for %q authentication", message.Auth.Kind)
	}
	return message, nil
}

func main() {
	if err := disableCrashDumps(); err != nil {
		log.Fatal("disable crash dumps: ", err)
	}
	a := &Agent{
		sess:     map[string]*Session{},
		channels: map[string]channelEntry{},
		out:      json.NewEncoder(os.Stdout),
	}
	workers := make(chan struct{}, 64)
	var requests sync.WaitGroup
	reader := bufio.NewReaderSize(os.Stdin, 64*1024)
	for {
		m, err := readControlMessage(reader)
		if errors.Is(err, io.EOF) {
			break
		}
		if err != nil {
			a.send(Msg{Ok: false, Err: err.Error()})
			if strings.Contains(err.Error(), "secret frame") || strings.Contains(err.Error(), "control frame") {
				break
			}
			continue
		}
		workers <- struct{}{}
		requests.Add(1)
		go func() {
			defer func() { <-workers }()
			defer requests.Done()
			defer m.Auth.clear()
			a.handle(m)
		}()
	}
	requests.Wait()
}
