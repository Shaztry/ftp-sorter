# FTP & Local File Sorter

A lightweight GUI utility built in Rust using `egui` for categorizing and moving files cleanly across FTP connections or your local storage.

![Rust](https://img.shields.io/badge/Rust-1.75%2B-orange?logo=rust)
![License](https://img.shields.io/badge/License-MIT-blue)

---

## Features

- **Dual Source Modes**:
  - **FTP Remote**: Connects and mounts remote FTP targets using `rclone`.
  - **Local Machine**: Fast local file sorting restricted strictly within `/home` boundaries for safe operation.
- **Smart Automatic Categorization**:
  - Sorts files into logical subdirectories (`documents/`, `images/`, `videos/`, `audio/`, `archives/`, `spreadsheets/`, `presentations/`, `fonts/`, `game_mods/`, `extras/`).
  - Automatic pattern matching for specialized keywords and file naming styles.
- **Safety First**:
  - **Non-destructive**: Never overwrites existing files; auto-increments collisions (`file (1).ext`).
  - **Git Safe**: Automatically skips scanning directories containing `.git` repos.
  - **Path Guard**: Blocks local execution attempts outside the user's home path.
- **Interactive UI & Live Logs**:
  - Built-in live progress tracking with real-time lane animations.
  - Terminal log viewer embedded directly into the interface.
  - Theme toggle (Dark / Light) and font configuration support.

---

## Prerequisites (Linux / Arch Linux)

Make sure the following runtime dependencies are installed for FTP remote mounting support:

```bash
# Arch Linux / Garuda Linux
sudo pacman -S rclone fuse3