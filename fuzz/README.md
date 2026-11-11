# VT parser fuzzing

The target compiles `native/vtparse.c` directly and feeds arbitrary bytes in
changing chunk sizes so streaming-boundary bugs are exercised too.

```sh
rustup toolchain install nightly --profile minimal
cargo install cargo-fuzz
cargo +nightly fuzz run vt_parser -- -dict=fuzz/dictionaries/vt_parser.dict
```

On Windows, install LLVM and Visual Studio C++ Build Tools with its
AddressSanitizer component, then run:

```powershell
powershell -ExecutionPolicy Bypass -File fuzz/run-vt.ps1 -max_total_time=60
```

The runner locates the Microsoft ASan DLL and places it first on `PATH` for
this process. The upstream LLVM DLL lacks Microsoft-specific entry points
required by the libFuzzer objects, even when its LLVM version matches.
Both the Rust harness and the C parser are instrumented for AddressSanitizer
and coverage. The runner restores `PATH` when finished.
