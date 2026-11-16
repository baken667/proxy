// Tells the app which commit it is built from and what its tunnel helper is
// built from, and gives the Windows .exe its icon.
fn main() {
    let git = std::process::Command::new("git").args(["rev-parse", "--short", "HEAD"]).output();
    let commit = git.ok().filter(|out| out.status.success()).map(|out| String::from_utf8_lossy(&out.stdout).trim().to_string());
    // Built outside a checkout (a source archive): there is no commit to name.
    println!("cargo:rustc-env=BKN_COMMIT={}", commit.as_deref().unwrap_or("unknown"));
    // A commit moves HEAD or a branch under refs, and changes no source file.
    println!("cargo:rerun-if-changed=.git/HEAD");
    println!("cargo:rerun-if-changed=.git/refs");
    println!("cargo:rerun-if-changed=assets/icon.ico");

    // What the tunnel helper is built from: its sources and the versions of
    // everything it links. The macOS app compares this with the installed
    // helper's and reinstalls it (a password prompt) only when they differ,
    // not on every update.
    // ponytail: helper code outside these two files is not seen; add the file
    // here if the helper ever grows one. Any dependency bump counts as a change.
    let mut hasher = std::hash::DefaultHasher::new();
    for file in ["src/tunnel.rs", "src/net.rs", "Cargo.lock"] {
        let text = std::fs::read_to_string(file).unwrap_or_default();
        // Not this package's own entry in Cargo.lock: its version changes with every release.
        for part in text.split("[[package]]").filter(|part| !part.contains("\nname = \"bkn-proxy\"\n")) {
            std::hash::Hash::hash(part, &mut hasher);
        }
        println!("cargo:rerun-if-changed={file}");
    }
    println!("cargo:rustc-env=BKN_HELPER_BUILD={:016x}", std::hash::Hasher::finish(&hasher));

    #[cfg(windows)]
    winresource::WindowsResource::new().set_icon("assets/icon.ico").compile().expect("embed the icon");
}
