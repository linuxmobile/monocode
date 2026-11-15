//! The provider and model a new conversation starts with. One resolver, so
//! every way of starting a chat agrees.
//!
//! Order: project default, Settings default, last choice (`recent_models[0]`),
//! then Codex (or the first allowed provider). A step is skipped when its
//! provider is hidden. Hidden providers are ignored for remote projects, as
//! the model picker ignores them there.
use super::*;
use crate::settings::Preferences;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct NewChatChoice {
    pub kind: Kind,
    pub model: Option<String>,
}

impl NewChatChoice {
    /// A provider picked by the caller: the model follows project then Settings.
    pub(crate) fn explicit(prefs: &Preferences, cwd: &str, kind: Kind) -> Self {
        Self::of(kind, default_model(prefs, cwd, kind))
    }
    fn of(kind: Kind, model: Option<String>) -> Self {
        Self { kind, model }
    }
}

/// The project's model for the provider, else the Settings default.
fn default_model(prefs: &Preferences, cwd: &str, kind: Kind) -> Option<String> {
    prefs
        .project_providers
        .get(cwd)
        .and_then(|p| p.models.get(kind.id()))
        .or(prefs.default_models.get(kind.id()))
        .cloned()
}

fn allowed(prefs: &Preferences, cwd: &str, kind: Kind) -> bool {
    monocode_core::remote::parse_remote_path(cwd).is_some()
        || !prefs.hidden_for(cwd).iter().any(|id| id == kind.id())
}

pub(crate) fn resolve(prefs: &Preferences, cwd: &str) -> NewChatChoice {
    resolve_with(prefs, cwd, monocode_core::provider::installed)
}

/// `resolve` with the installation probe injected.
///
/// Step 4 turns on what is installed, so the tests must supply that answer
/// rather than inherit the developer's machine, which would make the suite pass
/// or fail depending on which CLIs happen to be present.
pub(crate) fn resolve_with(
    prefs: &Preferences,
    cwd: &str,
    installed: impl Fn(Kind) -> bool,
) -> NewChatChoice {
    // 1. The project's provider.
    // 2. The Settings provider.
    for id in [
        prefs
            .project_providers
            .get(cwd)
            .and_then(|p| p.provider.as_deref()),
        prefs.default_provider.as_deref(),
    ] {
        if let Some(kind) = id.and_then(Kind::parse).filter(|k| allowed(prefs, cwd, *k)) {
            return NewChatChoice::of(kind, default_model(prefs, cwd, kind));
        }
    }
    // 3. The last model picked anywhere.
    if let Some(recent) = prefs.recent_models.first()
        && let Some(kind) = Kind::parse(&recent.provider).filter(|k| allowed(prefs, cwd, *k))
    {
        return NewChatChoice::of(
            kind,
            default_model(prefs, cwd, kind).or_else(|| recent.model.clone()),
        );
    }
    // 4. Codex if it is installed, otherwise the first provider that is.
    //
    // Preferring Codex unconditionally made a fresh install on a machine
    // without it open a new chat that could never run, while an installed
    // provider sat unused; the status bar then showed the unusable choice as
    // disabled. Installation is checked before preference so the default is
    // always something that can actually start a turn.
    let candidates = [Kind::Codex]
        .into_iter()
        .chain(Kind::ALL)
        .filter(|k| allowed(prefs, cwd, *k));
    let kind = candidates
        .clone()
        .find(|k| installed(*k))
        // 5. Nothing is installed: name a provider so the UI can explain what
        // to install, rather than leaving the choice empty.
        .or_else(|| candidates.into_iter().next())
        .unwrap_or(Kind::Codex);
    NewChatChoice::of(kind, default_model(prefs, cwd, kind))
}

impl Shell {
    /// The choice for a new conversation in `cwd`.
    pub(super) fn new_chat_choice(&self, cwd: &str) -> NewChatChoice {
        resolve(&self.workspace.preferences, cwd)
    }
    /// A new chat in the current project.
    pub(super) fn default_choice(&self) -> NewChatChoice {
        self.new_chat_choice(&self.workspace.project)
    }
    /// A caller-picked provider in the current project.
    pub(super) fn explicit_choice(&self, kind: Kind) -> NewChatChoice {
        NewChatChoice::explicit(&self.workspace.preferences, &self.workspace.project, kind)
    }
}
