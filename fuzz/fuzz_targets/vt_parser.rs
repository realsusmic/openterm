#![no_main]

use libfuzzer_sys::fuzz_target;
use std::ffi::c_void;

type PrintCallback = unsafe extern "C" fn(*mut c_void, u32);
type ExecuteCallback = unsafe extern "C" fn(*mut c_void, u8);
type CsiCallback = unsafe extern "C" fn(*mut c_void, u8, *const u32, u8, *const u8, *const u8, u8);
type EscCallback = unsafe extern "C" fn(*mut c_void, u8, *const u8, u8);
type OscCallback = unsafe extern "C" fn(*mut c_void, *const u8, u32);
type DcsCallback =
    unsafe extern "C" fn(*mut c_void, u8, *const u32, u8, *const u8, *const u8, u8, *const u8, u32);

extern "C" {
    fn vt_sizeof() -> usize;
    fn vt_init(parser: *mut c_void);
    fn vt_set_callbacks(
        parser: *mut c_void,
        print: PrintCallback,
        execute: ExecuteCallback,
        csi: CsiCallback,
        esc: EscCallback,
        osc: OscCallback,
        dcs: DcsCallback,
        userdata: *mut c_void,
    );
    fn vt_feed(parser: *mut c_void, data: *const u8, len: usize);
}

unsafe extern "C" fn on_print(_: *mut c_void, _: u32) {}
unsafe extern "C" fn on_execute(_: *mut c_void, _: u8) {}
unsafe extern "C" fn on_csi(
    _: *mut c_void,
    _: u8,
    _: *const u32,
    _: u8,
    _: *const u8,
    _: *const u8,
    _: u8,
) {
}
unsafe extern "C" fn on_esc(_: *mut c_void, _: u8, _: *const u8, _: u8) {}
unsafe extern "C" fn on_osc(_: *mut c_void, _: *const u8, _: u32) {}
unsafe extern "C" fn on_dcs(
    _: *mut c_void,
    _: u8,
    _: *const u32,
    _: u8,
    _: *const u8,
    _: *const u8,
    _: u8,
    _: *const u8,
    _: u32,
) {
}

fuzz_target!(|data: &[u8]| {
    let words = (unsafe { vt_sizeof() } + 7) / 8;
    let mut storage = vec![0_u64; words];
    let parser = storage.as_mut_ptr().cast::<c_void>();
    unsafe {
        vt_init(parser);
        vt_set_callbacks(
            parser,
            on_print,
            on_execute,
            on_csi,
            on_esc,
            on_osc,
            on_dcs,
            std::ptr::null_mut(),
        );

        // Feed in changing chunk sizes as well as arbitrary bytes. Parser bugs
        // often hide specifically at escape/UTF-8 boundaries between reads.
        let mut offset = 0;
        let mut chunk = data.first().copied().unwrap_or(1) as usize % 31 + 1;
        while offset < data.len() {
            let end = (offset + chunk).min(data.len());
            vt_feed(parser, data[offset..end].as_ptr(), end - offset);
            offset = end;
            chunk = (chunk * 5 + 1) % 37 + 1;
        }
    }
});
