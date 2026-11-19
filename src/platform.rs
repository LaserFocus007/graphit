//! Was sich zwischen Linux und Windows unterscheidet: Hilfsprogramme starten und beenden.
//! Alles Windows-Spezifische steht hinter `cfg(windows)`, der Linux-Weg bleibt unverändert.

use std::ffi::OsStr;
use std::process::Command;

/// Kommandozeilenprogramm vorbereiten. Unter Windows ohne aufblitzendes Konsolenfenster.
pub fn command(program: impl AsRef<OsStr>) -> Command {
    #[allow(unused_mut)]
    let mut cmd = Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

/// Prozess beenden, unter Windows samt seinen Kindprozessen.
pub fn kill(pid: u32) {
    #[cfg(windows)]
    let _ = command("taskkill").args(["/PID", &pid.to_string(), "/T", "/F"]).status();
    #[cfg(not(windows))]
    let _ = Command::new("kill").arg(pid.to_string()).status();
}

/// Name einer ausführbaren Datei auf diesem System.
pub fn exe(name: &str) -> String {
    if cfg!(windows) { format!("{name}.exe") } else { name.to_string() }
}
