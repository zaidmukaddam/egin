// templates/, catalog/ and the docs are embedded in the binary; rebuild when any file in them changes.
fn main() {
    println!("cargo:rerun-if-changed=templates");
    println!("cargo:rerun-if-changed=catalog");
    println!("cargo:rerun-if-changed=site/src/content/docs");
}
