//! Opening a project folder in an installed code editor.
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::scm::shell::{apply_gui_env, expand_home, resolve_gui_binary};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExternalEditor {
    pub id: &'static str,
    pub name: &'static str,
}

struct EditorDefinition {
    id: &'static str,
    name: &'static str,
    commands: &'static [&'static str],
}

const EDITORS: &[EditorDefinition] = &[
    EditorDefinition {
        id: "vscode",
        name: "Visual Studio Code",
        commands: &["code"],
    },
    EditorDefinition {
        id: "vscode-insiders",
        name: "Visual Studio Code Insiders",
        commands: &["code-insiders"],
    },
    EditorDefinition {
        id: "vscodium",
        name: "VSCodium",
        commands: &["codium"],
    },
    EditorDefinition {
        id: "cursor",
        name: "Cursor",
        commands: &["cursor"],
    },
    EditorDefinition {
        id: "zed",
        name: "Zed",
        commands: &["zed"],
    },
    EditorDefinition {
        id: "windsurf",
        name: "Windsurf",
        commands: &["windsurf"],
    },
    EditorDefinition {
        id: "sublime-text",
        name: "Sublime Text",
        commands: &["subl", "sublime_text"],
    },
];

enum EditorLauncher {
    Command(PathBuf),
}

fn definition(id: &str) -> Option<&'static EditorDefinition> {
    EDITORS.iter().find(|editor| editor.id == id)
}

fn resolve_editor(editor: &EditorDefinition) -> Option<EditorLauncher> {
    editor
        .commands
        .iter()
        .find_map(|command| resolve_gui_binary(command))
        .map(EditorLauncher::Command)
}

/// Editors found on this machine. Blocking: the first call may read the
/// login shell's PATH, so callers run it on a background executor.
pub fn installed() -> Vec<ExternalEditor> {
    EDITORS
        .iter()
        .filter(|editor| resolve_editor(editor).is_some())
        .map(|editor| ExternalEditor {
            id: editor.id,
            name: editor.name,
        })
        .collect()
}

/// Opens the folder `cwd` in the editor `editor_id`. Blocking, like [`installed`].
pub fn open(editor_id: &str, cwd: &str) -> Result<(), String> {
    let editor = definition(editor_id).ok_or_else(|| "Unknown external editor.".to_string())?;
    let cwd = expand_home(Path::new(cwd));
    if !cwd.is_dir() {
        return Err(format!("{} is not a folder.", cwd.display()));
    }
    let launcher =
        resolve_editor(editor).ok_or_else(|| format!("{} is no longer installed.", editor.name))?;

    let mut command = match launcher {
        EditorLauncher::Command(program) => {
            let mut command = Command::new(program);
            command.arg(&cwd);
            command
        }
    };

    apply_gui_env(&mut command);
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(|_| ())
        .map_err(|error| format!("Could not open {}: {error}", editor.name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn editor_ids_and_names_are_unique() {
        let mut ids = std::collections::HashSet::new();
        let mut names = std::collections::HashSet::new();
        for editor in EDITORS {
            assert!(ids.insert(editor.id), "duplicate editor id: {}", editor.id);
            assert!(
                names.insert(editor.name),
                "duplicate editor name: {}",
                editor.name
            );
            assert!(!editor.commands.is_empty());
        }
    }

    #[test]
    fn unknown_editor_is_rejected_before_launch() {
        let error = open("not-an-editor", ".").unwrap_err();
        assert_eq!(error, "Unknown external editor.");
    }
}
