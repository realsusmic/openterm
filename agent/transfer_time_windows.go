package main

import (
	"os"
	"time"

	"golang.org/x/sys/windows"
)

func preserveDownloadTime(file *os.File, modified time.Time) {
	ft := windows.NsecToFiletime(modified.UnixNano())
	_ = windows.SetFileTime(windows.Handle(file.Fd()), nil, &ft, &ft)
}
