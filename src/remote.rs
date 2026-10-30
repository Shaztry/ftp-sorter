use crate::sorter::Msg;
use std::process::Command;
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::Sender,
        Arc,
    },
    thread,
    time::Duration,
};

/// Checks if rclone is installed, and if not, opens a terminal to install it.
pub fn ensure_rclone_installed() -> Result<(), String> {
    // 1. Check if rclone exists in PATH
    if Command::new("rclone").arg("version").output().is_ok() {
        return Ok(());
    }

    // 2. Detect available terminal emulator to run the install command
    let install_cmd = "sudo pacman -S --needed --noconfirm rclone || sudo apt-get update && sudo apt-get install -y rclone || curl https://rclone.org/install.sh | sudo bash";
    
    let status = if Command::new("konsole").arg("-v").output().is_ok() {
        Command::new("konsole")
            .args(["-e", "bash", "-c", &format!("{install_cmd}; echo 'Press Enter to continue...'; read")])
            .status()
    } else if Command::new("alacritty").arg("-V").output().is_ok() {
        Command::new("alacritty")
            .args(["-e", "bash", "-c", &format!("{install_cmd}; echo 'Press Enter to continue...'; read")])
            .status()
    } else if Command::new("x-terminal-emulator").arg("-v").output().is_ok() {
        Command::new("x-terminal-emulator")
            .args(["-e", "bash", "-c", &format!("{install_cmd}; echo 'Press Enter to continue...'; read")])
            .status()
    } else {
        // Fallback using pkexec / GUI elevation if no terminal binary matches
        Command::new("pkexec")
            .args(["pacman", "-S", "--noconfirm", "rclone"])
            .status()
    };

    match status {
        Ok(s) if s.success() || Command::new("rclone").arg("version").output().is_ok() => Ok(()),
        _ => Err("Failed to install rclone automatically. Please install it manually.".into()),
    }
}

#[derive(Clone)]
pub struct Conn {
    pub ip: String,
    pub port: String,
    pub user: String,
    pub pass: String,
}

fn cache_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
    PathBuf::from(home).join(".cache/ftp-sorter")
}

pub fn mount_dir() -> PathBuf {
    cache_dir().join("mnt")
}

fn is_mounted(dir: &Path) -> bool {
    let Ok(s) = fs::read_to_string("/proc/mounts") else { return false };
    let needle = format!(" {} ", dir.display());
    s.lines().any(|l| l.contains(&needle))
}

/// Disconnects the phone. This only detaches the mount, it deletes nothing.
pub fn unmount(dir: &Path) {
    if !is_mounted(dir) {
        return;
    }
    let _ = Command::new("fusermount3").arg("-u").arg(dir).output();
    for _ in 0..20 {
        if !is_mounted(dir) {
            break;
        }
        thread::sleep(Duration::from_millis(250));
    }
}

fn obscure(pass: &str) -> Result<String, String> {
    let mut child = Command::new("rclone")
        .args(["obscure", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("could not run rclone (is it installed?): {e}"))?;
    child
        .stdin
        .take()
        .unwrap()
        .write_all(pass.as_bytes())
        .map_err(|e| e.to_string())?;
    let out = child.wait_with_output().map_err(|e| e.to_string())?;
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Builds an rclone command that talks to this FTP server.
/// Settings go through environment variables so the password never shows up in `ps`.
fn rclone(conn: &Conn, obscured: &str) -> Command {
    let mut c = Command::new("rclone");
    c.env("RCLONE_FTP_HOST", conn.ip.trim())
        .env("RCLONE_FTP_PORT", conn.port.trim())
        .env("RCLONE_FTP_USER", conn.user.trim())
        .env("RCLONE_FTP_CONCURRENCY", "2");
    if !obscured.is_empty() {
        c.env("RCLONE_FTP_PASS", obscured);
    }
    c
}

fn last_line(s: &str) -> String {
    s.lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("unknown error")
        .trim()
        .to_string()
}

/// Test login, then mount. Returns the local folder the phone is mounted at.
pub fn connect(conn: &Conn, tx: &Sender<Msg>, cancel: &Arc<AtomicBool>) -> Result<PathBuf, String> {
    let log = |s: &str| {
        let _ = tx.send(Msg::Log(s.to_string()));
    };

    if let Err(e) = ensure_rclone_installed() {
        return Err(e);
    }

    if conn.ip.trim().is_empty() {
        return Err("IP is empty".into());
    }
    let obscured = if conn.pass.is_empty() { String::new() } else { obscure(&conn.pass)? };

    log(&format!("Connecting to ftp://{}:{} ...", conn.ip.trim(), conn.port.trim()));
    let out = rclone(conn, &obscured)
        .args([
            "lsd", ":ftp:", "--contimeout", "8s", "--timeout", "15s", "--retries", "1",
            "--low-level-retries", "1",
        ])
        .output()
        .map_err(|e| format!("could not run rclone: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "login/connection failed: {}",
            last_line(&String::from_utf8_lossy(&out.stderr))
        ));
    }
    if cancel.load(Ordering::Relaxed) {
        return Err("cancelled".into());
    }

    log("Login OK. Mounting phone storage...");
    let dir = mount_dir();
    unmount(&dir);
    fs::create_dir_all(&dir).map_err(|e| format!("cannot create mount folder: {e}"))?;

    // rclone's own messages go to this log (appended, never overwritten)
    let log_path = cache_dir().join("rclone.log");
    let logf = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .map_err(|e| e.to_string())?;

    let mut child = rclone(conn, &obscured)
        .args(["mount", ":ftp:"])
        .arg(&dir)
        .args(["--vfs-cache-mode", "writes", "--dir-cache-time", "5s", "--contimeout", "8s"])
        .stdin(Stdio::null())
        .stdout(Stdio::from(logf.try_clone().map_err(|e| e.to_string())?))
        .stderr(Stdio::from(logf))
        .spawn()
        .map_err(|e| format!("could not start rclone mount: {e}"))?;

    let mut ok = false;
    for _ in 0..60 {
        if cancel.load(Ordering::Relaxed) {
            unmount(&dir);
            return Err("cancelled".into());
        }
        if let Ok(Some(status)) = child.try_wait() {
            let tail = fs::read_to_string(&log_path).unwrap_or_default();
            return Err(format!("mount exited ({status}): {}", last_line(&tail)));
        }
        if is_mounted(&dir) {
            ok = true;
            break;
        }
        thread::sleep(Duration::from_millis(250));
    }
    // reap the child when it exits (after unmount) so it doesn't linger as a zombie
    thread::spawn(move || {
        let _ = child.wait();
    });
    if !ok {
        unmount(&dir);
        return Err(format!("mount timed out, see {}", log_path.display()));
    }
    fs::read_dir(&dir).map_err(|e| format!("mounted but cannot list files: {e}"))?;
    log(&format!("Mounted at {}", dir.display()));
    Ok(dir)
}
