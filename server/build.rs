// The server embeds the gateway, engine and server migrations at compile time.
// Rebuild when any of those folders changes, so `server migrate` never ships
// without a new migration.
fn main() {
    println!("cargo:rerun-if-changed=migrations");
    println!("cargo:rerun-if-changed=../gateway/migrations");
    println!("cargo:rerun-if-changed=../engine/migrations");
}
