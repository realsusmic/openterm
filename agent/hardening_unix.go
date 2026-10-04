//go:build !windows

package main

import "golang.org/x/sys/unix"

func disableCrashDumps() error {
	return unix.Setrlimit(unix.RLIMIT_CORE, &unix.Rlimit{Cur: 0, Max: 0})
}
