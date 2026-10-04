//go:build windows

package main

import "golang.org/x/sys/windows"

const (
	semFailCriticalErrors = 0x0001
	semNoGpFaultErrorBox  = 0x0002
	semNoOpenFileErrorBox = 0x8000
	werNoHeap             = 0x0001
)

func disableCrashDumps() error {
	windows.SetErrorMode(semFailCriticalErrors | semNoGpFaultErrorBox | semNoOpenFileErrorBox)
	// Keep heap pages (where framed credentials live) out of Windows Error
	// Reporting data when the host exports this optional WER API. Some Windows
	// editions omit the export; crash hardening must never prevent agent startup.
	wer := windows.NewLazySystemDLL("wer.dll").NewProc("WerSetFlags")
	if err := wer.Find(); err != nil {
		return nil
	}
	result, _, _ := wer.Call(werNoHeap)
	if int32(result) < 0 {
		return nil
	}
	return nil
}
