// Gives the Windows .exe its icon. Does nothing on other systems.
fn main() {
    #[cfg(windows)]
    winresource::WindowsResource::new().set_icon("assets/icon.ico").compile().expect("embed the icon");
}
