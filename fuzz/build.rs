fn main() {
    let mut build = cc::Build::new();
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
        build.compiler("clang-cl");
    } else {
        build.compiler("clang");
    }
    build
        .file("../native/vtparse.c")
        .include("../native")
        .flag("-fsanitize=address")
        .flag("-fsanitize-coverage=inline-8bit-counters,pc-table,trace-cmp")
        .warnings(true)
        .compile("vtparse_fuzz");
    println!("cargo:rerun-if-changed=../native/vtparse.c");
    println!("cargo:rerun-if-changed=../native/vtparse.h");
}
