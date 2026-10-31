use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::Sender,
        Arc,
    },
};
use walkdir::WalkDir;

pub enum Msg {
    Log(String),
    Progress(usize, usize), // done, total
    FoldersLoaded(Vec<String>),
    Scanned(Plan),
    ScanFailed(String),
    Done,
}

pub struct MoveItem {
    pub from: PathBuf,
    pub category: &'static str,
    pub sub: Option<String>, // e.g. Some("pdf") for documents/pdf
    pub size: u64,
}

impl MoveItem {
    /// destination folder relative to the root, e.g. "documents/pdf" or "images"
    pub fn dest_rel(&self) -> PathBuf {
        match &self.sub {
            Some(s) => Path::new(self.category).join(s),
            None => PathBuf::from(self.category),
        }
    }
}

#[derive(Default)]
pub struct Plan {
    pub root: PathBuf,
    pub moves: Vec<MoveItem>,
    pub by_cat: BTreeMap<&'static str, (usize, u64)>,
}

impl Plan {
    pub fn total_bytes(&self) -> u64 {
        self.by_cat.values().map(|v| v.1).sum()
    }
    /// rough guess for FTP renames; tune SECS_PER_MOVE after a real run
    pub fn eta_secs(&self) -> f64 {
        const SECS_PER_MOVE: f64 = 0.12;
        self.moves.len() as f64 * SECS_PER_MOVE
    }
}

fn is_digit_tilde_pattern(s: &str) -> bool {
    // matches *~[0-9]*~*
    let b = s.as_bytes();
    for i in 0..b.len().saturating_sub(1) {
        if b[i] == b'~' && b[i + 1].is_ascii_digit() && b[i + 2..].contains(&b'~') {
            return true;
        }
    }
    false
}

fn pattern_category(lower: &str) -> Option<&'static str> {
    if is_digit_tilde_pattern(lower) {
        return Some("extras");
    }
    if lower.contains("nagabhushan") || lower.contains("nagbhushan") {
        return Some("nagabhushan");
    }
    if lower.contains("chethana") {
        return Some("chethana");
    }
    None
}

const DOC_EXTS: &[&str] = &[
    "pdf", "doc", "docx", "odt", "txt", "rtf", "epub", "mobi", "md", "html", "css", "js",
    "json", "xml", "yaml", "yml", "ini", "cfg", "log", "sh", "py", "bat", "ps1", "lua",
    "robot",
];

fn ext_category(ext: &str) -> (&'static str, Option<String>) {
    let e = ext.to_lowercase();
    if DOC_EXTS.contains(&e.as_str()) {
        return ("documents", Some(e)); // common -> documents/<ext>
    }
    let cat = match e.as_str() {
        "xls" | "xlsx" | "csv" | "ods" => "spreadsheets",
        "ppt" | "pptx" | "odp" => "presentations",
        "jpg" | "jpeg" | "png" | "webp" | "heic" | "gif" | "svg" | "bmp" | "ico" | "tif"
        | "tiff" => "images",
        "mp4" | "mkv" | "avi" | "mov" | "wmv" | "dem" | "webm" | "flv" => "videos",
        "mp3" | "flac" | "wav" | "m4a" | "ogg" => "audio",
        "zip" | "7z" | "gz" | "tar" | "rar" | "bz2" | "xz" => "archives",
        "str2_pc" | "asm_pc" | "vpp_pc" | "xtbl" => "game_mods",
        "ttf" | "otf" | "woff" | "woff2" => "fonts",
        _ => "extras", // uncommon stuff
    };
    (cat, None)
}

fn categorize(name: &str) -> Option<(&'static str, Option<String>)> {
    let lower = name.to_lowercase();
    if [".crdownload", ".part", ".tmp", ".aria2"]
        .iter()
        .any(|e| lower.ends_with(e))
    {
        return None;
    }
    if let Some(c) = pattern_category(&lower) {
        return Some((c, None));
    }
    if name.starts_with('.') {
        return None;
    }
    let (stem, ext) = name.rsplit_once('.')?;
    if stem.is_empty() || ext.is_empty() {
        return None;
    }
    Some(ext_category(ext))
}

pub fn scan(root: &Path, tx: &Sender<Msg>, cancel: &Arc<AtomicBool>) -> Result<Plan, String> {
    if !root.is_dir() {
        return Err(format!("not a directory: {}", root.display()));
    }
    let root = root.canonicalize().map_err(|e| e.to_string())?;
    if root.join(".git").is_dir() {
        return Err("target is a Git repository, aborting".into());
    }
    let _ = tx.send(Msg::Log(format!("Scanning '{}' (skipping Git repos)...", root.display())));

    let mut plan = Plan { root: root.clone(), ..Default::default() };
    let walker = WalkDir::new(&root)
        .min_depth(1)
        .into_iter()
        .filter_entry(|e| !(e.file_type().is_dir() && e.path().join(".git").is_dir()));

    let mut seen = 0usize;
    for entry in walker.filter_map(Result::ok) {
        if cancel.load(Ordering::Relaxed) {
            return Err("scan cancelled".into());
        }
        seen += 1;
        if seen % 500 == 0 {
            let _ = tx.send(Msg::Log(format!("  ...{seen} entries checked")));
        }
        if !entry.file_type().is_file() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        let Some((cat, sub)) = categorize(&name) else { continue };
        let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
        let item = MoveItem { from: entry.path().to_path_buf(), category: cat, sub, size };
        if entry.path().parent() == Some(root.join(item.dest_rel()).as_path()) {
            continue; // already in the right folder
        }
        let slot = plan.by_cat.entry(cat).or_insert((0, 0));
        slot.0 += 1;
        slot.1 += item.size;
        plan.moves.push(item);
    }
    let _ = tx.send(Msg::Log(format!(
        "Scan done: {} entries checked, {} files can be moved.",
        seen,
        plan.moves.len()
    )));
    Ok(plan)
}

/// Never overwrites, never deletes: clashes get "name (1).ext", "name (2).ext", ...
fn free_name(dir: &Path, name: &str) -> PathBuf {
    let first = dir.join(name);
    if !first.exists() {
        return first;
    }
    let (stem, ext) = match name.rsplit_once('.') {
        Some((s, e)) => (s.to_string(), format!(".{e}")),
        None => (name.to_string(), String::new()),
    };
    let mut n = 1;
    loop {
        let p = dir.join(format!("{stem} ({n}){ext}"));
        if !p.exists() {
            return p;
        }
        n += 1;
    }
}

pub fn run(plan: &Plan, tx: &Sender<Msg>, cancel: &Arc<AtomicBool>) {
    let total = plan.moves.len();
    for (i, m) in plan.moves.iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            let _ = tx.send(Msg::Log(format!("Cancelled after {i} of {total} files.")));
            let _ = tx.send(Msg::Done);
            return;
        }
        let name = m.from.file_name().unwrap().to_string_lossy().to_string();
        let dest_dir = plan.root.join(m.dest_rel());
        let result = fs::create_dir_all(&dest_dir)
            .and_then(|_| fs::rename(&m.from, free_name(&dest_dir, &name)));
        let line = match result {
            Ok(_) => format!("Moved: '{name}' -> '{}/'", m.dest_rel().display()),
            Err(e) => format!("FAILED: '{name}': {e} (left untouched)"),
        };
        let _ = tx.send(Msg::Log(line));
        let _ = tx.send(Msg::Progress(i + 1, total));
    }
    let _ = tx.send(Msg::Log("Consolidation complete! (no folders or files deleted)".into()));
    let _ = tx.send(Msg::Done);
}

pub fn list_folders(path: &std::path::Path) -> std::io::Result<Vec<String>> {
    let mut folders = Vec::new();
    if !path.exists() || !path.is_dir() {
        return Ok(folders);
    }
    for entry in std::fs::read_dir(path)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            if let Some(name) = entry.file_name().to_str() {
                if !name.starts_with('.') {
                    folders.push(name.to_string());
                }
            }
        }
    }
    folders.sort();
    Ok(folders)
}
