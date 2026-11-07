/// Data directory used when `portable.txt` or `.portable` sits next to the
/// executable. Same marker the desktop app checks before it starts.
pub fn portable_data_dir_beside(exe: &std::path::Path) -> Option<std::path::PathBuf> {
    let dir = exe.parent()?;
    if dir.join("portable.txt").exists() || dir.join(".portable").exists() {
        Some(dir.join("data"))
    } else {
        None
    }
}

pub fn app_data_dir() -> Option<std::path::PathBuf> {
    if let Ok(dir) = std::env::var("OMNIGET_DATA_DIR") {
        return Some(std::path::PathBuf::from(dir));
    }

    let base = dirs::data_dir()?;
    let new_path = base.join("com.openselena.omniget");
    let legacy_paths = [base.join("omniget")];

    for old_path in &legacy_paths {
        if old_path.exists() {
            let _ = std::fs::create_dir_all(&new_path);

            for dir_name in &["bin", "plugins"] {
                let src = old_path.join(dir_name);
                let dst = new_path.join(dir_name);
                if src.exists() && !dst.exists() {
                    let _ = copy_dir_recursive(&src, &dst);
                }
            }

            if let Ok(entries) = std::fs::read_dir(old_path) {
                for entry in entries.flatten() {
                    if entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
                        let dest = new_path.join(entry.file_name());
                        if !dest.exists() {
                            let _ = std::fs::copy(entry.path(), &dest);
                        }
                    }
                }
            }
        }
    }

    Some(new_path)
}

fn copy_dir_recursive(src: &std::path::Path, dst: &std::path::Path) -> std::io::Result<()> {
    if src.is_dir() {
        std::fs::create_dir_all(dst)?;
        for entry in std::fs::read_dir(src)? {
            let entry = entry?;
            let dest = dst.join(entry.file_name());
            copy_dir_recursive(&entry.path(), &dest)?;
        }
    } else {
        std::fs::copy(src, dst)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn portable_marker_selects_the_data_dir_beside_the_exe() {
        let root = std::env::temp_dir().join(format!("omniget-portable-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let exe = root.join("omniget-cli");
        fs::write(&exe, b"").unwrap();
        assert!(portable_data_dir_beside(&exe).is_none());
        fs::write(root.join("portable.txt"), b"").unwrap();
        assert_eq!(portable_data_dir_beside(&exe).unwrap(), root.join("data"));
        let _ = fs::remove_dir_all(&root);
    }
}
