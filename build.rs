// Tells the app which commit it is built from, and gives the Windows .exe its icon.
fn main() {
    let git = std::process::Command::new("git").args(["rev-parse", "--short", "HEAD"]).output();
    let commit = git.ok().filter(|out| out.status.success()).map(|out| String::from_utf8_lossy(&out.stdout).trim().to_string());
    // Built outside a checkout (a source archive): there is no commit to name.
    println!("cargo:rustc-env=BKN_COMMIT={}", commit.as_deref().unwrap_or("unknown"));
    // A commit moves HEAD or a branch under refs, and changes no source file.
    println!("cargo:rerun-if-changed=.git/HEAD");
    println!("cargo:rerun-if-changed=.git/refs");
    println!("cargo:rerun-if-changed=assets/icon.ico");

    #[cfg(windows)]
    winresource::WindowsResource::new().set_icon("assets/icon.ico").compile().expect("embed the icon");
}
