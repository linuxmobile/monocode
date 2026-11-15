//! Platform shell selection, independent of the terminal renderer.

pub(super) fn pty_program(program: &str) -> String {
    // Alacritty escapes arguments on Windows, but inserts the program verbatim
    // into CreateProcessW's command line. PowerShell 7 lives under Program Files.

    program.to_owned()
}

pub(super) fn default_shell(_search_path: &str) -> (String, Vec<String>) {
    let shell = std::env::var("SHELL")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "/bin/bash".into());
    let stem = std::path::Path::new(&shell)
        .file_stem()
        .and_then(|n| n.to_str())
        .unwrap_or_default();
    let args = match stem {
        "zsh" | "bash" | "sh" | "fish" => vec!["-l".to_owned()],
        _ => Vec::new(),
    };
    (shell, args)
}
