//! Ce qui diffère entre Linux et Windows : ouvrir un dossier, montrer un fichier, corbeille,
//! fenêtres de console des programmes lancés.

use std::path::Path;
use std::process::Command;

/// Ouvre un dossier dans le gestionnaire de fichiers.
pub fn open_dir(dir: &Path) {
    #[cfg(windows)]
    let _ = Command::new("explorer").arg(dir).spawn();
    #[cfg(not(windows))]
    let _ = Command::new("xdg-open").arg(dir).spawn();
}

/// Ouvre un fichier avec l'application associée (lecteur PDF…).
pub fn open_file(file: &Path) {
    #[cfg(windows)]
    let _ = Command::new("explorer").arg(file).spawn();
    #[cfg(not(windows))]
    let _ = Command::new("xdg-open").arg(file).spawn();
}

/// Montre un fichier sélectionné dans l'Explorateur. Faux si ce n'est pas géré ici (Linux : D-Bus).
pub fn select_in_explorer(_file: &Path) -> bool {
    #[cfg(windows)]
    {
        let mut arg = std::ffi::OsString::from("/select,");
        arg.push(_file.as_os_str());
        return Command::new("explorer").arg(arg).spawn().is_ok();
    }
    #[cfg(not(windows))]
    false
}

/// Sous Windows, un programme en ligne de commande lancé depuis l'application n'ouvre pas de console.
pub fn no_console(cmd: &mut Command) -> &mut Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}
