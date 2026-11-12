// CI sets VESSEL_VERSION to the release version (e.g. 0.1.3); rebuild when it changes.
fn main() {
    println!("cargo:rerun-if-env-changed=VESSEL_VERSION");
}
