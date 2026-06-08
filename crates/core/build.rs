use std::env;
use std::fs;
use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-env-changed=CODESEEX_MODEL_CATALOG_SEED");

    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let private_default = manifest_dir
        .join("..")
        .join("..")
        .join(".private")
        .join("model-catalog.seed.json");

    let seed_path = env::var_os("CODESEEX_MODEL_CATALOG_SEED")
        .map(PathBuf::from)
        .unwrap_or(private_default);
    println!("cargo:rerun-if-changed={}", seed_path.display());

    if !seed_path.is_file() {
        panic!(
            "CodeSeeX model catalog seed is not stored in public source. \
Set CODESEEX_MODEL_CATALOG_SEED or place model-catalog.seed.json under .private to build release binaries."
        );
    }

    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR"));
    fs::copy(seed_path, out_dir.join("model-catalog.seed.json"))
        .expect("copy private model catalog seed");
}
