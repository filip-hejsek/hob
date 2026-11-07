//! Links the system libjxl, found with pkg-config.

fn main() {
    for lib in ["libjxl", "libjxl_threads"] {
        if let Err(e) = pkg_config::Config::new().atleast_version("0.8").probe(lib) {
            panic!("kettle-jxl needs {lib} (from libjxl 0.8 or newer): {e}");
        }
    }
}
