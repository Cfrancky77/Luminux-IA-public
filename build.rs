// Windows : icône et informations de version intégrées au .exe (Explorateur, barre des tâches).
fn main() {
    println!("cargo:rerun-if-changed=packaging/luminux.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("packaging/luminux.ico");
        res.set("ProductName", "Luminux IA");
        res.set("FileDescription", "Luminux IA");
        if let Err(e) = res.compile() {
            println!("cargo:warning=icône Windows non intégrée : {e}");
        }
    }
}
