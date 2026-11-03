use std::path::Path;

/// The portable package keeps config next to MetaDefender.app, rather than in
/// Finder's unspecified working directory. CLI binaries retain the caller's cwd.
pub fn package_root(executable: &Path) -> Option<&Path> {
    let macos = executable.parent()?;
    let contents = macos.parent()?;
    let bundle = contents.parent()?;
    if macos.file_name()? != "MacOS"
        || contents.file_name()? != "Contents"
        || bundle.extension()? != "app"
    {
        return None;
    }
    bundle.parent()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finder_bundle_uses_adjacent_config_without_changing_cli_resolution() {
        let root = Path::new("測試套件");
        assert_eq!(
            package_root(&root.join("MetaDefender.app/Contents/MacOS/client")),
            Some(root)
        );
        assert_eq!(package_root(Path::new("target/release/client")), None);
        assert_eq!(package_root(Path::new("bin/client")), None);
        assert_eq!(package_root(Path::new("Other/Contents/MacOS/client")), None);
    }
}
