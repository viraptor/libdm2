fn main() {
    // Try LZFSE_DIR first (explicit), then pkg-config, then hope it's in the system path
    if let Ok(dir) = std::env::var("LZFSE_DIR") {
        println!("cargo:rustc-link-search=native={dir}/lib");
    } else if let Ok(lib) = pkg_config::probe_library("liblzfse") {
        for path in &lib.link_paths {
            println!("cargo:rustc-link-search=native={}", path.display());
        }
    }
    println!("cargo:rustc-link-lib=lzfse");
}
