//go:build unix

package main

import (
	"os"
	"time"

	"golang.org/x/sys/unix"
)

func preserveDownloadTime(file *os.File, modified time.Time) {
	tv := unix.NsecToTimeval(modified.UnixNano())
	_ = unix.Futimes(int(file.Fd()), []unix.Timeval{tv, tv})
}
