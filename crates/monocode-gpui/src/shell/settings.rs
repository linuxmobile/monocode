//! The settings surface replaces the rail body and the workspace, preserving
//! open conversations. Its breadcrumb/actions occupy the inset title bar.
use super::*;
use crate::settings::{ArchivedSession, Section, SettingsContext, SettingsEvent, SettingsView};
use std::path::Path;
impl Shell {
    fn settings_context(&self, cx: &App) -> SettingsContext {
        let mut archived: Vec<_> = self
            .order
            .iter()
            .map(|&id| self.session_metadata(id, cx).summary)
            .filter(|s| s.archived && s.cwd == self.workspace.project)
            .map(|s| ArchivedSession {
                id: s.id,
                title: s.title.clone(),
                provider: s.provider.clone(),
                updated_at: s.updated_at,
            })
            .collect();
        archived.sort_by_key(|s| std::cmp::Reverse(s.updated_at));
        let mut catalogs = HashMap::new();
        let active = self
            .monos
            .as_ref()
            .filter(|_| self.mono_active(cx))
            .and_then(|m| m.read(cx).chat())
            .or_else(|| {
                self.workspace.active.and_then(|id| {
                    self.sessions
                        .get(&self.workspace.session_for_pane(id))
                        .cloned()
                })
            });
        let mut profiles: HashMap<_, _> = Kind::ALL
            .into_iter()
            .map(|kind| {
                (
                    kind,
                    crate::accounts::get(cx)
                        .map(|center| center.read(cx).selected(kind, &self.workspace.project).id)
                        .unwrap_or_else(|| monocode_core::accounts::DEFAULT.into()),
                )
            })
            .collect();
        if let Some(active) = &active {
            let v = active.read(cx);
            profiles.insert(
                Kind::from_id(&v.summary.provider),
                v.settings.account.id.clone(),
            );
        }
        for session in active.iter().chain(self.sessions.values()) {
            let session = session.read(cx);
            if profiles.get(&Kind::from_id(&session.summary.provider))
                != Some(&session.settings.account.id)
            {
                continue;
            }
            if catalogs.contains_key(&session.summary.provider) {
                continue;
            }
            let models = session.settings_models();
            if !models.is_empty() {
                catalogs.insert(session.summary.provider.clone(), models);
            }
        }
        SettingsContext {
            remote: Some(self.remote.clone()),
            quick_error: self.quick.error.clone(),
            inbox_connections: self
                .inbox
                .as_ref()
                .map(|inbox| inbox.read(cx).connection_statuses())
                .unwrap_or_default(),
            sessions: self
                .order
                .iter()
                .map(|&id| {
                    (
                        id,
                        self.session_working_copy(id, cx).to_owned(),
                        self.session_metadata(id, cx).busy,
                    )
                })
                .collect(),
            monos: self
                .monos
                .as_ref()
                .map(|m| {
                    m.read(cx)
                        .roster
                        .monos
                        .iter()
                        .map(|m| crate::settings::MonoSetting {
                            id: m.id.clone(),
                            name: m.label(),
                            mascot: m.mascot.clone(),
                            color: m.color,
                            projects: m.projects.iter().map(|p| self.project_label(p)).collect(),
                            show: m.show_started_sessions_in_sidebar,
                            folders: m.use_sidebar_folders,
                        })
                        .collect()
                })
                .unwrap_or_default(),
            catalogs,
            classic: self.workspace.classic_layout,
            new_layout_boxy: self.workspace.new_layout_boxy,
            cwd: self.workspace.project.clone(),
            project_label: self.project_label(&self.workspace.project),
            archived,
            archived_projects: self
                .workspace
                .archived_projects
                .iter()
                .map(|path| (path.clone(), self.project_label(path)))
                .collect(),
            projects: self
                .workspace
                .projects
                .iter()
                .map(|path| (path.clone(), self.project_label(path)))
                .collect(),
        }
    }
    pub(super) fn settings_page_open(&self) -> bool {
        self.settings_shown && self.settings.is_some()
    }
    pub(super) fn open_settings(
        &mut self,
        section: Option<Section>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.leave_app_search(cx);
        self.close_preview(cx);
        self.menu = None;
        if self.settings.is_none() {
            self.create_settings(section.unwrap_or_default(), window, cx);
            self.settings_nav_visible = true;
        } else if let Some(section) = section {
            self.select_settings(section, cx);
        }
        self.show_settings(window, cx);
    }
    fn create_settings(&mut self, section: Section, window: &mut Window, cx: &mut Context<Self>) {
        self.navigation_revision += 1;
        let context = self.settings_context(cx);
        let prefs = self.workspace.preferences.clone();
        let view = cx.new(|cx| SettingsView::new(section, prefs, context, cx));
        self.settings_events = Some(cx.subscribe_in(
            &view,
            window,
            |this, _, event: &SettingsEvent, window, cx| match event {
                SettingsEvent::Section(section) => this.select_settings(*section, cx),
                SettingsEvent::ManageInboxProvider(provider) => {
                    this.open_inbox(window, cx);
                    if let Some(inbox) = &this.inbox {
                        inbox.update(cx, |inbox, cx| inbox.open_connection(provider.clone(), cx));
                    }
                }
                SettingsEvent::WhatsNew => {
                    this.open_whats_new(crate::notifications::updater::version().into(), cx)
                }
                SettingsEvent::CheckAppUpdates => {
                    this.notification_state
                        .updater
                        .update(cx, |updater, cx| updater.check(true, cx));
                }
                SettingsEvent::CheckHarnessUpdates => {
                    let hidden = this.workspace.preferences.hidden_providers.clone();
                    this.notification_state
                        .harness
                        .update(cx, |notice, cx| notice.check_now(hidden, cx));
                }
                SettingsEvent::Changed(prefs) => {
                    if this.workspace.preferences.wayland_window_decorations
                        != prefs.wayland_window_decorations
                        && let Some(decorations) = prefs.wayland_decorations(cx.compositor_name())
                    {
                        window.request_decorations(decorations);
                    }
                    this.workspace.preferences = *prefs.clone();
                    this.apply_preferences(cx);
                    this.persist(cx);
                    this.invalidate_chrome(cx);
                    cx.notify();
                }
                SettingsEvent::NewLayout(on) => {
                    if this.workspace.classic_layout == *on {
                        this.toggle_layout(cx);
                    }
                    this.sync_settings(cx);
                }
                SettingsEvent::NewLayoutBoxy(boxy) => {
                    this.workspace.new_layout_boxy = *boxy;
                    this.layout_changed(cx);
                    this.sync_settings(cx);
                }
                SettingsEvent::OpenSession(id) => {
                    this.close_settings(window, cx);
                    this.open(*id, window, cx);
                }
                SettingsEvent::Archive(id, archived) => {
                    if this.session_summaries.contains_key(id)
                        && this.session_metadata(*id, cx).summary.archived != *archived
                    {
                        this.toggle_archive(*id, cx);
                    }
                }
                SettingsEvent::Delete(id) => this.delete_from_settings(*id, window, cx),
                SettingsEvent::UnarchiveProject(path) => this.unarchive_project(path, cx),
                SettingsEvent::DeleteProject(path) => {
                    this.confirm_delete_project(path.clone(), window, cx)
                }
                SettingsEvent::RemoveWorktree {
                    cwd,
                    path,
                    delete_sessions,
                } => this.remove_settings_worktree(
                    cwd.clone(),
                    path.clone(),
                    *delete_sessions,
                    window,
                    cx,
                ),
                SettingsEvent::ChooseBackground => this.choose_chat_background(cx),
                SettingsEvent::ResetMono(id) => {
                    if let Some(m) = this.monos.clone() {
                        let reset = m.update(cx, |m, cx| m.reset_defaults(id, cx));
                        cx.spawn(async move |this, cx| {
                            let result = reset.await;
                            let _ = this.update(cx, |this, cx| {
                                if let Some(settings) = &this.settings {
                                    settings.update(cx, |s, cx| s.management_finished(result, cx));
                                }
                                this.sync_settings(cx);
                            });
                        })
                        .detach();
                    }
                }
                SettingsEvent::MonoSidebar(id, show, folders) => {
                    if let Some(m) = this.monos.clone() {
                        m.update(cx, |m, cx| m.sidebar_preferences(id, *show, *folders, cx));
                    }
                    this.sync_settings(cx);
                }
            },
        ));
        // This dialog is painted at the window root, like React's portal.
        // Redraw it for countdown/copy/close changes even when only the
        // Settings entity was notified, without rebuilding the shell for
        // ordinary settings edits.
        // The search lives in the cached rail, so redraw it as the query moves.
        let mut search_was = view.read(cx).search_state();
        self.settings_overlay_watch = Some(cx.observe(&view, move |this, view, cx| {
            let search = view.read(cx).search_state();
            if search != search_was {
                search_was = search;
                this.invalidate_chrome(cx);
                cx.notify();
            }
        }));
        self.settings = Some(view);
    }
    pub(super) fn show_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(view) = self.settings.clone() else {
            return;
        };
        if !self.settings_shown {
            self.settings_restore = window.focused(cx);
            self.leave_app_views(cx);
            self.leave_app_search(cx);
            self.close_preview(cx);
            if let Some(monos) = self.monos.clone() {
                monos.update(cx, |monos, cx| monos.close(cx));
            }
            self.review_focused = false;
        }
        self.settings_shown = true;
        self.sidebar_slide = None;
        self.sidebar_fraction = if self.settings_nav_visible { 1. } else { 0. };
        self.remember_title_tab(TitleTab::Settings);
        self.reveal_active_tab(cx);
        window.focus(&view.focus_handle(cx), cx);
        self.sync_settings(cx);
        self.update_chat_visibility(cx);
        self.sync_right_panel(cx);
        self.invalidate_chrome(cx);
        self.persist(cx);
        cx.notify();
    }
    pub(super) fn hide_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.settings_shown {
            return;
        }
        self.settings_shown = false;
        self.sidebar_slide = None;
        self.sidebar_fraction = if self.workspace.sidebar_visible {
            1.
        } else {
            0.
        };
        if let Some(restore) = self.settings_restore.take() {
            window.focus(&restore, cx);
        }
        self.update_chat_visibility(cx);
        self.sync_right_panel(cx);
        self.invalidate_chrome(cx);
        cx.notify();
    }
    pub(super) fn clear_settings(&mut self) {
        self.settings = None;
        self.settings_shown = false;
        self.settings_restore = None;
        self.settings_events = None;
        self.settings_overlay_watch = None;
        self.sidebar_slide = None;
        self.sidebar_fraction = if self.workspace.sidebar_visible {
            1.
        } else {
            0.
        };
    }
    pub(super) fn select_settings(&mut self, section: Section, cx: &mut Context<Self>) {
        if let Some(view) = &self.settings {
            view.update(cx, |v, cx| v.select_section(section, cx));
        }
        this_update(self, cx);
    }
    pub(super) fn sync_settings(&mut self, cx: &mut Context<Self>) {
        if let Some(view) = self.settings.clone() {
            let context = self.settings_context(cx);
            let prefs = self.workspace.preferences.clone();
            view.update(cx, |v, cx| {
                v.set_context(context, cx);
                v.set_preferences(&prefs, cx);
            });
        }
    }
    pub(super) fn close_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.settings.is_none() {
            return;
        }
        let restore = self.settings_restore.take();
        self.close_title_tabs(&[TitleTab::Settings], None, window, cx);
        if self.settings_page_open() {
            return;
        }
        match restore {
            Some(handle) => window.focus(&handle, cx),
            None => self.focus_active(window, cx),
        }
    }
    pub(super) fn apply_preferences(&mut self, cx: &mut Context<Self>) {
        self.refresh_notification_permission();
        self.configure_quick_composer(cx);
        // Switching search styles closes an open search of the other style.
        if self
            .app_search
            .as_ref()
            .is_some_and(|s| s.panel != self.workspace.preferences.search_panel)
        {
            self.leave_app_search(cx);
            self.update_chat_visibility(cx);
            self.invalidate_chrome(cx);
        }
        monocode_core::cli::initialize(&self.workspace.preferences.binary_paths);
        self.keep_visible_idle_agents(cx);
        monocode_core::provider::configure_idle_agents(
            self.workspace.preferences.idle_agents as usize,
        );
        monocode_core::accounts::set_claude_entrypoint(
            self.workspace.preferences.claude_entrypoint,
        );
        if let Some(monos) = &self.monos {
            let prefs = self.workspace.preferences.clone();
            monos.update(cx, |m, cx| m.set_preferences(prefs, cx));
        }
        crate::settings::apply_keybindings(&self.workspace.preferences, cx);
        cx.set_global(self.workspace.preferences.clone());
        self.sync_right_panel(cx);
        if let Some(center) = crate::accounts::get(cx) {
            center.update(cx, |center, cx| {
                let changed = center.mask_emails != self.workspace.preferences.mask_emails
                    || center.show_remaining != self.workspace.preferences.show_remaining_usage;
                center.mask_emails = self.workspace.preferences.mask_emails;
                center.show_remaining = self.workspace.preferences.show_remaining_usage;
                if changed {
                    cx.notify();
                }
            });
        }
        for session in self.sessions.values() {
            session.read(cx).transcript.clone().update(cx, |view, cx| {
                view.apply_preferences(&self.workspace.preferences, cx)
            });
        }
        self.usage_footer.update(cx, |footer, cx| {
            footer.set_remaining(self.workspace.preferences.show_remaining_usage, cx)
        });
    }
    fn choose_chat_background(&mut self, cx: &mut Context<Self>) {
        let prompt = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: None,
        });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(paths))) = prompt.await
                && let Some(path) = paths.first()
            {
                let source = path.clone();
                let directory = crate::data_dir().join("backgrounds");
                let work = cx.background_executor().spawn(async move {
                    monocode_core::project_appearance::import_image(
                        &source, &directory, "global", false,
                    )
                });
                let result = work.await;
                let _ = this.update_in(cx, |this, window, cx| match result {
                    Ok(path) => {
                        this.workspace.preferences.chat_background = Some(path);
                        this.persist(cx);
                        this.sync_settings(cx);
                        cx.notify();
                    }
                    Err(error) => {
                        let prompt = window.prompt(
                            gpui_kit::PromptLevel::Critical,
                            "Cannot use this image",
                            Some(&error),
                            &["OK"],
                            cx,
                        );
                        cx.spawn(async move |_, _| {
                            let _ = prompt.await;
                        })
                        .detach();
                    }
                });
            }
        })
        .detach();
    }
    fn finish_worktree_action(&mut self, result: Result<(), String>, cx: &mut Context<Self>) {
        if let Some(view) = &self.settings {
            view.update(cx, |v, cx| v.management_finished(result, cx));
        }
        self.sync_git(cx);
        self.sync_settings(cx);
        self.invalidate_chrome(cx);
        cx.notify();
    }
    fn remove_settings_worktree(
        &mut self,
        cwd: String,
        path: String,
        delete_sessions: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use monocode_core::files::is_equal_or_inside;
        if self
            .terminals
            .values()
            .flatten()
            .any(|t| is_equal_or_inside(&t.read(cx).cwd, &path))
        {
            self.finish_worktree_action(
                Err("Close the terminals using this worktree first.".into()),
                cx,
            );
            return;
        }
        let ids = self
            .session_summaries
            .keys()
            .filter(|id| is_equal_or_inside(self.session_working_copy(**id, cx), &path))
            .copied()
            .collect::<Vec<_>>();
        if ids
            .iter()
            .any(|id| self.sessions.get(id).is_some_and(|v| v.read(cx).busy))
        {
            self.finish_worktree_action(
                Err("Stop the running sessions using this worktree first.".into()),
                cx,
            );
            return;
        }
        let check_cwd = cwd.clone();
        let check_path = path.clone();
        let work = cx.background_executor().spawn(async move {
            monocode_core::worktree_management::check(Path::new(&check_cwd), Path::new(&check_path))
        });
        cx.spawn(async move|this,cx|{
            let result=work.await;
            if let Err(e)=result {let _=this.update(cx,|this,cx|this.finish_worktree_action(Err(e),cx));return;}
            let pending=match this.update_in(cx,|this,window,cx| {
                // Recheck before deleting any conversations after the preflight worker.
                if this.terminals.values().flatten().any(|t|is_equal_or_inside(&t.read(cx).cwd,&path))||ids.iter().any(|id|this.sessions.get(id).is_some_and(|s|s.read(cx).busy)) {return Err("This worktree is now in use. Close its terminals and stop its sessions first.".to_owned());}
                if delete_sessions {for id in &ids{this.delete_session(*id,window,cx);}}
                Ok((this.group_writes.take(),this.store.clone()))
            }) {Ok(Ok(v))=>v,Ok(Err(e))=>{let _=this.update(cx,|this,cx|this.finish_worktree_action(Err(e),cx));return;},Err(_)=>return};
            if let Some(task)=pending.0 {task.await;}
            let worker_cwd=cwd.clone();let worker_path=path.clone();let store=pending.1;
            let work=cx.background_executor().spawn(async move{
                if delete_sessions {let store=locked(&store)?;let paths=store.session_worktrees()?;if store.sessions()?.iter().any(|s|is_equal_or_inside(paths.get(&s.id).and_then(|p|p.as_deref()).unwrap_or(&s.cwd),&worker_path)){return Err("Some sessions could not be deleted, so the worktree was kept.".into());}}
                let snapshot=monocode_core::git::snapshot(Path::new(&worker_cwd))?;
                let main=snapshot.worktrees.first().ok_or("No main working copy found")?;
                monocode_core::worktree_management::remove(Path::new(&main.path),Path::new(&worker_path))?;
                locked(&store)?.detach_worktree(&worker_path,&main.path)
            });let result=work.await;
            let _=this.update(cx,|this,cx|{let result=result.map(|detached|{for(summary,settings)in detached{this.session_summaries.insert(summary.id, summary.clone());this.session_worktrees.insert(summary.id,settings.worktree_cwd.clone());if let Some(view)=this.sessions.get(&summary.id){view.update(cx,|v,cx|v.worktree_deleted(summary,settings,cx));}}this.worktree_focus.retain(|_,focus|!is_equal_or_inside(&focus.path,&path));});this.finish_worktree_action(result,cx);});
        }).detach();
        let _ = window;
    }
    fn delete_from_settings(&mut self, id: i64, window: &mut Window, cx: &mut Context<Self>) {
        let prompt = window.prompt(
            gpui_kit::PromptLevel::Warning,
            "Delete this conversation permanently?",
            Some("Its messages and saved settings will be removed."),
            &["Delete", "Cancel"],
            cx,
        );
        cx.spawn(async move |this, cx| {
            if prompt.await != Ok(0) {
                return;
            }
            let _ = this.update_in(cx, |this, window, cx| {
                this.delete_session(id, window, cx);
            });
        })
        .detach();
    }
    pub(super) fn settings_title_tab(
        &self,
        width: f32,
        cx: &mut Context<Self>,
    ) -> gpui_kit::Stateful<gpui_kit::Div> {
        let active = self.settings_shown;
        let close = active || width >= TAB_CLOSE_MIN_WIDTH;
        div()
            .id("settings-title-tab")
            .debug_selector(|| "settings-title-tab".into())
            .group("settings-title-tab")
            .role(Role::Tab)
            .aria_label("Settings")
            .aria_selected(active)
            .cursor_pointer()
            .w(px(width))
            .h(px(TITLE_BAR_HEIGHT))
            .flex_shrink_0()
            .flex()
            .items_center()
            .relative()
            .child(
                title_tab_plate(active, false)
                    .text_color(content(if active { 1. } else { 0.5 }))
                    .w_full()
                    .min_w_0()
                    .when(close && width >= TAB_CLOSE_MIN_WIDTH, |d| d.pr_7())
                    .when(width < NARROW_TAB, |d| {
                        d.px(px(((width - TAB_TILE.w) / 2.).max(0.)))
                    })
                    .gap(px(TAB_TILE_GAP))
                    .group_hover("settings-title-tab", |s| {
                        s.text_color(content(1.)).bg(title_tab_fill(active, true))
                    })
                    .child(mark_tile(
                        &TAB_TILE,
                        Face::Neutral,
                        icon("settings", 14.).text_color(crate::tab_tile::tile_ink()),
                    ))
                    .child(
                        div()
                            .min_w_0()
                            .flex_1()
                            .truncate()
                            .text_size(px(TITLE_TAB_TEXT_SIZE))
                            .child("Settings"),
                    ),
            )
            .when(close, |d| {
                d.child(
                    div()
                        .id("close-settings-tab")
                        .debug_selector(|| "close-settings-tab".into())
                        .role(Role::Button)
                        .aria_label("Close Settings")
                        .absolute()
                        .right_1()
                        .top(px(10.))
                        .size(px(20.))
                        .rounded_sm()
                        .flex()
                        .items_center()
                        .justify_center()
                        .opacity(0.)
                        .group_hover("settings-title-tab", |s| s.opacity(1.))
                        .hover(|s| s.bg(content(0.1)))
                        .child(icon("close", 12.))
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .on_click(cx.listener(|this, _, window, cx| {
                            cx.stop_propagation();
                            this.close_settings(window, cx);
                        })),
                )
            })
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event, _, cx| {
                    this.start_tab_drag(TitleTab::Settings, event, cx)
                }),
            )
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(|this, event: &MouseDownEvent, window, cx| {
                    this.open_menu(
                        MenuTarget::SettingsTab,
                        Anchor::Point(event.position),
                        window,
                        cx,
                    )
                }),
            )
            .on_mouse_up(
                MouseButton::Middle,
                cx.listener(|this, _, window, cx| {
                    cx.stop_propagation();
                    this.close_settings(window, cx);
                }),
            )
            .on_click(cx.listener(|this, _, window, cx| {
                if std::mem::take(&mut this.skip_tab_click) {
                    return;
                }
                this.show_settings(window, cx);
            }))
    }
}
fn this_update(this: &mut Shell, cx: &mut Context<Shell>) {
    this.invalidate_chrome(cx);
    this.update_chat_visibility(cx);
    cx.notify();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::tests::window;
    use gpui_kit::TestAppContext;

    #[gpui_kit::test]
    fn settings_skill_form_opens_and_cancels_without_leaving_settings(cx: &mut TestAppContext) {
        cx.update(super::super::bind_keys);
        let (window, _, _) = window(cx);
        window
            .update(cx, |shell, w, cx| {
                shell.open_settings(Some(Section::Skills), w, cx)
            })
            .unwrap();
        let visual = &mut gpui_kit::VisualTestContext::from_window(window.into(), cx);
        visual.run_until_parked();
        let add = visual.debug_bounds("settings-add-skill").unwrap();
        visual.simulate_click(add.center(), Default::default());
        visual.run_until_parked();
        assert!(visual.debug_bounds("settings-skill-form").is_some());
        assert!(visual.debug_bounds("settings-management-dialog").is_none());
        visual.simulate_keystrokes("escape");
        visual.run_until_parked();
        assert!(visual.debug_bounds("settings-skill-form").is_none());
        window
            .update(visual, |shell, _, _| assert!(shell.settings.is_some()))
            .unwrap();
    }

    #[gpui_kit::test]
    fn escape_closes_the_inner_settings_dialog_first(cx: &mut TestAppContext) {
        cx.update(super::super::bind_keys);
        let (window, _, _) = window(cx);
        window
            .update(cx, |shell, w, cx| {
                shell.open_settings(Some(Section::Mcp), w, cx)
            })
            .unwrap();
        let visual = &mut gpui_kit::VisualTestContext::from_window(window.into(), cx);
        visual.run_until_parked();
        let add = visual.debug_bounds("settings-add-mcp").unwrap();
        visual.simulate_click(add.center(), Default::default());
        visual.run_until_parked();
        assert!(visual.debug_bounds("settings-management-dialog").is_some());
        visual.simulate_keystrokes("escape");
        visual.run_until_parked();
        window
            .update(visual, |shell, _, _| assert!(shell.settings.is_some()))
            .unwrap();
        assert!(visual.debug_bounds("settings-management-dialog").is_none());
        visual.simulate_keystrokes("escape");
        visual.run_until_parked();
        window
            .update(visual, |shell, _, _| assert!(shell.settings.is_none()))
            .unwrap();
    }
    #[gpui_kit::test]
    fn reopening_settings_with_the_shortcut_keeps_the_section(cx: &mut TestAppContext) {
        cx.update(|cx| {
            super::super::bind_keys(cx);
            crate::input::bind(cx);
            crate::popover::bind(cx);
        });
        cx.update(|cx| cx.set_reduce_motion(true));
        let (window, _, _) = window(cx);
        window
            .update(cx, |shell, w, cx| shell.open_settings(None, w, cx))
            .unwrap();
        let visual = &mut gpui_kit::VisualTestContext::from_window(window.into(), cx);
        visual.run_until_parked();
        let keybindings = visual.debug_bounds("settings-nav-keybindings").unwrap();
        visual.simulate_click(keybindings.center(), Default::default());
        visual.run_until_parked();
        window
            .update(visual, |shell, _, cx| {
                let view = shell.settings.clone().unwrap();
                assert_eq!(view.read(cx).section, Section::Keybindings);
            })
            .unwrap();
        visual.simulate_keystrokes("secondary-,");
        visual.run_until_parked();
        window
            .update(visual, |shell, _, cx| {
                let view = shell.settings.clone().unwrap();
                assert_eq!(view.read(cx).section, Section::Keybindings);
                assert!(shell.settings_page_open());
                assert_eq!(shell.active_title_tab(cx), Some(TitleTab::Settings));
            })
            .unwrap();
    }
    #[gpui_kit::test]
    fn settings_tab_survives_switching_to_an_agent_tab(cx: &mut TestAppContext) {
        cx.update(|cx| {
            super::super::bind_keys(cx);
            crate::input::bind(cx);
            crate::popover::bind(cx);
        });
        cx.update(|cx| cx.set_reduce_motion(true));
        let (window, _, _) = window(cx);
        window
            .update(cx, |shell, w, cx| {
                shell.open_settings(Some(Section::Appearance), w, cx);
            })
            .unwrap();
        let visual = &mut gpui_kit::VisualTestContext::from_window(window.into(), cx);
        visual.run_until_parked();
        assert!(visual.debug_bounds("settings-title-tab").is_some());
        window
            .update(visual, |shell, w, cx| {
                assert_eq!(shell.active_title_tab(cx), Some(TitleTab::Settings));
                assert_eq!(
                    shell.settings.clone().unwrap().read(cx).section,
                    Section::Appearance
                );
                let session = shell
                    .visible_tabs(cx)
                    .into_iter()
                    .find_map(|tab| tab.session())
                    .unwrap();
                shell.open_title_tab(&TitleTab::Session(session), w, cx);
            })
            .unwrap();
        visual.run_until_parked();
        window
            .update(visual, |shell, w, cx| {
                assert!(shell.settings.is_some(), "the page stays open");
                assert!(!shell.settings_page_open());
                assert_ne!(shell.active_title_tab(cx), Some(TitleTab::Settings));
                shell.show_settings(w, cx);
            })
            .unwrap();
        visual.run_until_parked();
        window
            .update(visual, |shell, _, cx| {
                assert!(shell.settings_page_open());
                assert_eq!(
                    shell.settings.clone().unwrap().read(cx).section,
                    Section::Appearance
                );
            })
            .unwrap();
        assert!(visual.debug_bounds("settings-nav-appearance").is_some());
    }
    #[gpui_kit::test]
    fn settings_nav_search_steps_through_results_with_arrows(cx: &mut TestAppContext) {
        cx.update(|cx| {
            super::super::bind_keys(cx);
            crate::input::bind(cx);
            crate::popover::bind(cx);
        });
        cx.update(|cx| cx.set_reduce_motion(true));
        let (window, _, _) = window(cx);
        window
            .update(cx, |shell, w, cx| {
                shell.open_settings(Some(Section::General), w, cx)
            })
            .unwrap();
        let visual = &mut gpui_kit::VisualTestContext::from_window(window.into(), cx);
        visual.run_until_parked();
        let back = visual.debug_bounds("settings-nav-back").unwrap();
        let search = visual.debug_bounds("settings-search").unwrap();
        let first = visual.debug_bounds("settings-nav-general").unwrap();
        assert!(search.bottom() < first.top() && first.bottom() < back.top());
        visual.simulate_click(search.center(), Default::default());
        visual.simulate_input("theme");
        visual.run_until_parked();
        assert!(visual.debug_bounds("settings-result-1").is_some());
        visual.simulate_keystrokes("down");
        visual.run_until_parked();
        window
            .update(visual, |shell, _, cx| {
                assert_eq!(
                    shell.settings.as_ref().unwrap().read(cx).search_state().1,
                    1
                )
            })
            .unwrap();
    }
    #[gpui_kit::test]
    fn settings_replace_native_rail_and_tabs_and_return_to_the_same_session(
        cx: &mut TestAppContext,
    ) {
        cx.update(super::super::bind_keys);
        let (window, _, active) = window(cx);
        window
            .update(cx, |shell, w, cx| shell.focus_active(w, cx))
            .unwrap();
        let visual = &mut gpui_kit::VisualTestContext::from_window(window.into(), cx);
        visual.simulate_keystrokes("secondary-,");
        visual.run_until_parked();
        let card = visual.debug_bounds("main-pane-frame").unwrap();
        let header = visual.debug_bounds("settings-header").unwrap();
        let page = visual.debug_bounds("settings-page-header").unwrap();
        let tab = visual.debug_bounds("settings-title-tab").unwrap();
        assert!(
            tab.bottom() <= px(40.) && tab.origin.x >= card.left(),
            "the page has its own tab in the strip"
        );
        assert!(
            header.origin.y >= card.origin.y,
            "breadcrumb and search head the page"
        );
        assert!(
            (page.left()
                - card.left()
                - (card.size.width - px(8.) - px(1024.)).max(px(0.)) / 2.
                - px(33.))
            .abs()
                < px(1.),
            "React's 32px page padding plus the card border: page={page:?}, frame={card:?}"
        );
        assert!(visual.debug_bounds("settings-toggle-sounds").is_some());
        assert!(visual.debug_bounds("sidebar-content").is_none());
        for (section, selector) in Section::ALL.into_iter().zip([
            "settings-nav-general",
            "settings-nav-connections",
            "settings-nav-appearance",
            "settings-nav-keybindings",
            "settings-nav-chat",
            "settings-nav-providers",
            "settings-nav-mcp",
            "settings-nav-skills",
            "settings-nav-monos",
            "settings-nav-inbox",
            "settings-nav-archive",
            "settings-nav-worktrees",
        ]) {
            let bounds = visual.debug_bounds(selector).unwrap();
            visual.simulate_click(bounds.center(), Default::default());
            visual.run_until_parked();
            window
                .update(visual, |shell, _, cx| {
                    assert_eq!(shell.settings.as_ref().unwrap().read(cx).section, section)
                })
                .unwrap();
            assert!(
                visual.debug_bounds("settings-page-header").is_some(),
                "page {} renders",
                section.label()
            );
        }
        let back = visual.debug_bounds("settings-nav-back").unwrap();
        visual.simulate_click(back.center(), Default::default());
        visual.run_until_parked();
        window
            .update(visual, |shell, _, _| {
                assert!(shell.settings.is_none());
                assert_eq!(shell.workspace.active, Some(active));
                assert_eq!(shell.workspace.tabs.len(), 2);
            })
            .unwrap();
        assert!(visual.debug_bounds("sidebar-content").is_some());
    }

    #[gpui_kit::test]
    fn settings_nav_collapses_with_search_on_top_and_back_at_the_bottom(cx: &mut TestAppContext) {
        cx.update(super::super::bind_keys);
        cx.update(|cx| cx.set_reduce_motion(true));
        let (window, _, _) = window(cx);
        window
            .update(cx, |shell, w, cx| shell.focus_active(w, cx))
            .unwrap();
        let visual = &mut gpui_kit::VisualTestContext::from_window(window.into(), cx);
        visual.simulate_keystrokes("secondary-,");
        visual.run_until_parked();
        let back = visual.debug_bounds("settings-nav-back").unwrap();
        let search = visual.debug_bounds("settings-search").unwrap();
        let general = visual.debug_bounds("settings-nav-general").unwrap();
        let header = visual.debug_bounds("settings-header").unwrap();
        assert!(
            search.bottom() <= general.top(),
            "search heads the nav, above the pages"
        );
        assert!(general.bottom() <= back.top(), "Back sits below the pages");
        assert!(search.right() <= header.left(), "search lives in the nav");

        let toggle = visual.debug_bounds("toggle-sidebar").unwrap();
        visual.simulate_click(toggle.center(), Default::default());
        visual.run_until_parked();
        window
            .update(visual, |shell, _, _| {
                assert!(shell.settings.is_some());
                assert_eq!(shell.sidebar_fraction, 0.);
                assert!(
                    shell.workspace.sidebar_visible,
                    "the project sidebar is untouched"
                );
            })
            .unwrap();
        let search = visual.debug_bounds("settings-search").unwrap();
        let header = visual.debug_bounds("settings-header").unwrap();
        assert!(
            search.left() >= header.left() && search.bottom() <= header.bottom(),
            "collapsed, search moves to the title bar"
        );

        let toggle = visual.debug_bounds("toggle-sidebar").unwrap();
        visual.simulate_click(toggle.center(), Default::default());
        visual.run_until_parked();
        window
            .update(visual, |shell, _, _| assert_eq!(shell.sidebar_fraction, 1.))
            .unwrap();
    }

    #[gpui_kit::test]
    fn native_setting_edits_persist_and_theme_switches_live(cx: &mut TestAppContext) {
        let (window, _, _) = window(cx);
        window
            .update(cx, |shell, w, cx| {
                shell.open_settings(Some(Section::Appearance), w, cx)
            })
            .unwrap();
        let visual = &mut gpui_kit::VisualTestContext::from_window(window.into(), cx);
        visual.run_until_parked();
        let light = visual.debug_bounds("settings-theme-light").unwrap();
        visual.simulate_click(light.center(), Default::default());
        visual.run_until_parked();
        window
            .update(visual, |shell, _, _| {
                assert_eq!(shell.workspace.preferences.theme, "light");
                let saved = shell.latest_state.lock().unwrap().clone();
                let restored: Workspace = serde_json::from_str(&saved).unwrap();
                assert_eq!(restored.preferences.theme, "light");
                assert!(
                    content(1.).l < 0.3,
                    "the native text ink switched to the light palette"
                );
            })
            .unwrap();
        let glass = visual
            .debug_bounds("settings-toggle-main-pane-glass")
            .unwrap();
        assert!(glass.size.width > px(0.));
        let dark = visual.debug_bounds("settings-theme-dark").unwrap();
        visual.simulate_click(dark.center(), Default::default());
        visual.run_until_parked();
    }

    #[gpui_kit::test]
    fn shortcut_recording_rebinds_native_settings_and_reset_restores_it(cx: &mut TestAppContext) {
        cx.update(crate::input::bind);
        cx.update(super::super::bind_keys);
        let (window, _, _) = window(cx);
        window
            .update(cx, |shell, w, cx| {
                shell.open_settings(Some(Section::Keybindings), w, cx)
            })
            .unwrap();
        let visual = &mut gpui_kit::VisualTestContext::from_window(window.into(), cx);
        visual.run_until_parked();
        let recorder = visual.debug_bounds("shortcut-App: Settings").unwrap();
        visual.simulate_click(recorder.center(), Default::default());
        visual.simulate_keystrokes("secondary-k");
        visual.run_until_parked();
        window
            .update(visual, |shell, _, _| {
                assert!(
                    !shell
                        .workspace
                        .preferences
                        .keybindings
                        .contains_key("App: Settings"),
                    "a default shortcut cannot be claimed twice"
                )
            })
            .unwrap();
        visual.simulate_keystrokes("secondary-alt-y");
        visual.run_until_parked();
        window
            .update(visual, |shell, _, _| {
                assert!(
                    shell.workspace.preferences.keybindings["App: Settings"]
                        .as_ref()
                        .unwrap()
                        .ends_with('y')
                )
            })
            .unwrap();
        visual.simulate_keystrokes("escape");
        visual.run_until_parked();
        window
            .update(visual, |shell, _, _| assert!(shell.settings.is_none()))
            .unwrap();
        visual.simulate_keystrokes("secondary-,");
        visual.run_until_parked();
        window
            .update(visual, |shell, _, _| {
                assert!(shell.settings.is_none(), "old chord is removed")
            })
            .unwrap();
        visual.simulate_keystrokes("secondary-alt-y");
        visual.run_until_parked();
        window
            .update(visual, |shell, _, _| {
                assert!(
                    shell.settings.is_some(),
                    "the newly recorded chord dispatches the native action"
                )
            })
            .unwrap();
        window
            .update(visual, |shell, _, cx| {
                shell.select_settings(Section::Keybindings, cx)
            })
            .unwrap();
        visual.run_until_parked();
        let reset = visual.debug_bounds("reset-shortcut-App: Settings").unwrap();
        visual.simulate_click(reset.center(), Default::default());
        visual.run_until_parked();
        visual.simulate_keystrokes("escape");
        visual.run_until_parked();
        visual.simulate_keystrokes("secondary-,");
        visual.run_until_parked();
        window
            .update(visual, |shell, _, _| {
                assert!(
                    shell.settings.is_some(),
                    "reset restores the native default shortcut"
                )
            })
            .unwrap();
    }
}
