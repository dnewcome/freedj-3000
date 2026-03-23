fn main() {
    // Link against the system Rubber Band library (librubberband-dev).
    // pkg-config is the canonical way; fall back to a raw -lrubberband if unavailable.
    let lib = pkg_config::Config::new()
        .atleast_version("3.0.0")
        .probe("rubberband");

    match lib {
        Ok(_) => {
            // pkg-config emitted the right -L / -l flags automatically.
        }
        Err(e) => {
            // Fallback: just tell the linker to find -lrubberband on the standard path.
            eprintln!("cargo:warning=pkg-config failed ({e}); falling back to -lrubberband");
            println!("cargo:rustc-link-lib=rubberband");
        }
    }

    // Rubber Band is a C++ library; also link the C++ standard library.
    println!("cargo:rustc-link-lib=stdc++");
}
