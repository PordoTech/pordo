use super::*;
use updates::key;

impl App {
    pub(in crate::app) fn video_library_tabs(&mut self, ui: &mut Ui) {
        let l = self.settings.language;
        let (creators, videos) = self.online_video.updates.counts(&self.library.creators);
        let tip = (creators > 0 || videos > 0).then(|| {
            format!(
                "{} {creators} · {} {videos}",
                l.text("有更新的 UP 主", "Creators with updates"),
                l.text("未看视频", "Unseen videos")
            )
        });
        let top = ui.cursor().min;
        let right = ui.max_rect().right();
        let previous = self.workspace.video_tab;
        design::tabs_with(
            ui,
            &mut self.workspace.video_tab,
            &[
                design::TabItem {
                    label: l.text("收藏", "Favorites"),
                    badge: false,
                    tooltip: None,
                },
                design::TabItem {
                    label: l.text("搜索", "Search"),
                    badge: false,
                    tooltip: None,
                },
                design::TabItem {
                    label: l.text("更新", "Updates"),
                    badge: creators > 0,
                    tooltip: tip.as_deref(),
                },
            ],
        );
        if previous != self.workspace.video_tab {
            self.online_video.cancel_discovery();
            self.online_video.updates.return_to_updates = false;
        }
        let rect = Rect::from_min_size(
            pos2(right - design::CONTROL_HEIGHT, top.y),
            Vec2::splat(design::CONTROL_HEIGHT),
        );
        let mut actions = workspace::child(ui, "video-check-updates", rect);
        let state = &self.online_video.updates;
        let running = state.running();
        let tip = if running {
            format!(
                "{} {}/{}",
                l.text("检查更新中", "Checking updates"),
                state.done,
                state.total
            )
        } else {
            l.text("检查收藏 UP 主更新", "Check favorite creators for updates")
                .into()
        };
        if design::icon_button_loading(&mut actions, icons::ARROWS_CLOCKWISE, &tip, running)
            .clicked()
            && !running
        {
            self.queue_video_updates(true, None);
        }
    }
    fn open_update_video(&mut self, bvid: &str, title: &str) {
        if let Some(api) = self.online_video.api.clone() {
            let id = bvid.to_owned();
            self.begin_bilibili_playback(
                Job::spawn(move |_| async move { api.detail(&id).await }),
                1,
                None,
                title.into(),
            );
        } else {
            let record = library::Record {
                path: format!("https://www.bilibili.com/video/{bvid}"),
                title: title.into(),
                ..Default::default()
            };
            self.open_bilibili_record(bvid.into(), &record, false);
        }
        self.workspace.video_tab = 2;
    }
    fn open_creator_from_updates(&mut self, creator: &library::CreatorRecord) {
        self.online_video.updates.return_to_updates = true;
        self.open_bilibili_creator(creator.uploader.clone());
    }

    fn update_creator_menu(
        &mut self,
        ui: &mut Ui,
        creator: &library::CreatorRecord,
        ids: &[String],
    ) {
        let l = self.settings.language;
        if design::menu_item(ui, false, l.text("查看投稿", "View uploads")).clicked() {
            self.open_creator_from_updates(creator);
            ui.close();
        }
        ui.add_enabled_ui(!ids.is_empty(), |ui| {
            if design::menu_item(ui, false, l.text("全部标为已看", "Mark all as seen")).clicked()
            {
                self.online_video
                    .updates
                    .mark_read(&key(creator), ids.to_vec());
                ui.close();
            }
        });
        if design::menu_item(ui, false, l.text("重新检查", "Check again")).clicked() {
            self.queue_video_updates(true, Some(&key(creator)));
            ui.close();
        }
    }

    fn update_group_header(
        &mut self,
        ui: &mut Ui,
        creator: &library::CreatorRecord,
        id: &str,
        ids: &[String],
        collapsed: bool,
    ) {
        let l = self.settings.language;
        egui::Frame::new()
            .fill(SURFACE)
            .stroke(Stroke::new(1.0, design::LINE))
            .corner_radius(8)
            .inner_margin(egui::Margin::symmetric(10, 6))
            .show(ui, |ui| {
                ui.set_min_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 8.0;
                    if design::compact_icon_button(
                        ui,
                        if collapsed {
                            icons::CARET_RIGHT
                        } else {
                            icons::CARET_DOWN
                        },
                        l.text("展开 / 收起", "Expand / collapse"),
                        false,
                    )
                    .clicked()
                    {
                        if collapsed {
                            self.online_video.updates.collapsed.remove(id);
                        } else {
                            self.online_video.updates.collapsed.insert(id.to_owned());
                        }
                    }
                    let (avatar, avatar_resp) =
                        ui.allocate_exact_size(Vec2::splat(32.0), Sense::click());
                    let texture = self.online_video.previews.texture(
                        ui,
                        avatar,
                        creator.uploader.avatar.as_deref(),
                        true,
                    );
                    design::initial_avatar(ui, avatar, texture.as_ref(), &creator.uploader.uname);
                    if avatar_resp
                        .on_hover_cursor(egui::CursorIcon::PointingHand)
                        .on_hover_text(l.text("查看投稿", "View uploads"))
                        .clicked()
                    {
                        self.open_creator_from_updates(creator);
                    }

                    let badge_text = format!("{} {}", ids.len(), l.text("未看", "unseen"));
                    let badge_width = ui
                        .painter()
                        .layout_no_wrap(badge_text.clone(), egui::FontId::proportional(12.0), MUTED)
                        .size()
                        .x;
                    let mark_label = l.text("全部标为已看", "Mark all as seen");
                    let mark_galley_w = ui
                        .painter()
                        .layout_no_wrap(
                            mark_label.into(),
                            egui::TextStyle::Button.resolve(ui.style()),
                            design::TEXT,
                        )
                        .size()
                        .x;
                    let mark_width = (mark_galley_w + 20.0)
                        .ceil()
                        .max(design::COMPACT_CONTROL_HEIGHT);
                    let show_mark = ui.available_width() >= mark_width + badge_width + 130.0;
                    let right_reserve =
                        badge_width + 38.0 + if show_mark { mark_width + 16.0 } else { 0.0 };
                    let name_width = (ui.available_width() - right_reserve).max(48.0);
                    let (name_rect, name_resp) =
                        ui.allocate_exact_size(vec2(name_width, 32.0), Sense::click());
                    let name_color = if name_resp.hovered() {
                        ACCENT
                    } else {
                        design::TEXT
                    };
                    let mut name_job = egui::text::LayoutJob::simple(
                        creator.uploader.uname.clone(),
                        design::strong_font_id(ui, 14.5),
                        name_color,
                        name_width,
                    );
                    name_job.wrap.max_rows = 1;
                    name_job.wrap.overflow_character = Some('…');
                    let name_galley = ui.painter().layout_job(name_job);
                    let name_clip = name_rect.intersect(ui.clip_rect());
                    ui.painter().with_clip_rect(name_clip).galley(
                        pos2(
                            name_rect.min.x,
                            name_rect.center().y - name_galley.size().y * 0.5,
                        ),
                        name_galley,
                        name_color,
                    );
                    if name_resp
                        .on_hover_cursor(egui::CursorIcon::PointingHand)
                        .on_hover_text(&creator.uploader.uname)
                        .clicked()
                    {
                        self.open_creator_from_updates(creator);
                    }

                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.spacing_mut().item_spacing.x = 8.0;
                        let more = design::compact_icon_button(
                            ui,
                            icons::DOTS_THREE,
                            l.text("更多", "More"),
                            false,
                        );
                        egui::Popup::menu(&more)
                            .show(|ui| self.update_creator_menu(ui, creator, ids));
                        if show_mark
                            && design::compact_control(
                                ui,
                                mark_label,
                                None,
                                design::Control::Outline,
                            )
                            .clicked()
                        {
                            self.online_video.updates.mark_read(id, ids.to_vec());
                        }
                        ui.label(RichText::new(badge_text).small().color(MUTED));
                    });
                });
            });
    }

    fn update_post_card<Tz: chrono::TimeZone>(
        &mut self,
        ui: &mut Ui,
        creator_id: &str,
        post: &updates::Post,
        width: f32,
        now: &chrono::DateTime<Tz>,
    ) {
        let l = self.settings.language;
        let response = cards::video_card(
            ui,
            &mut self.online_video.previews,
            &post.video(),
            width,
            l,
            false,
            now,
        );
        let cover_height = (width - 16.0) * 9.0 / 16.0;
        let cover = Rect::from_min_size(
            response.rect.min + vec2(8.0, 8.0),
            vec2(width - 16.0, cover_height),
        );
        let dot_pos = cover.min + vec2(12.0, 12.0);
        ui.painter()
            .circle_filled(dot_pos, 4.5, Color32::from_black_alpha(190));
        ui.painter().circle_filled(dot_pos, 3.0, ACCENT);
        let menu_rect = Rect::from_min_size(
            pos2(cover.max.x - 40.0, cover.min.y + 8.0),
            Vec2::splat(32.0),
        );
        let popup_id = ui.id().with("video-update-post-menu");
        let menu_open = egui::Popup::is_id_open(ui.ctx(), popup_id);
        if menu_open
            || response.hovered()
            || response.has_focus()
            || ui.rect_contains_pointer(menu_rect)
        {
            let button = design::cover_overlay_button(
                ui,
                "video-update-menu",
                menu_rect,
                icons::DOTS_THREE,
                l.text("更多", "More"),
                menu_open,
            );
            egui::Popup::menu(&button).id(popup_id).show(|ui| {
                if design::menu_item(ui, false, l.text("标为已看", "Mark as seen")).clicked() {
                    self.online_video
                        .updates
                        .mark_read(creator_id, vec![post.bvid.clone()]);
                    ui.close();
                }
            });
        }
        response.context_menu(|ui| {
            if design::menu_item(ui, false, l.text("标为已看", "Mark as seen")).clicked() {
                self.online_video
                    .updates
                    .mark_read(creator_id, vec![post.bvid.clone()]);
                ui.close();
            }
        });
        if response.clicked() {
            self.open_update_video(&post.bvid, &post.title);
        }
    }

    fn update_compact_row(
        &mut self,
        ui: &mut Ui,
        creator: &library::CreatorRecord,
        id: &str,
        error: Option<&str>,
        pending: bool,
        paused: bool,
    ) {
        let l = self.settings.language;
        let (status_text, is_error) = if let Some(err) = error {
            (err.to_owned(), true)
        } else if paused {
            (
                l.text("仍有投稿待补查", "More uploads remain to check")
                    .into(),
                false,
            )
        } else if pending && !self.video_updates_cloud_ready() {
            (
                l.text(
                    "等待首次云同步，恢复投稿基线",
                    "Waiting for cloud sync to restore the baseline",
                )
                .into(),
                false,
            )
        } else {
            (
                l.text("尚未完成首次检查", "First update check not completed")
                    .into(),
                false,
            )
        };
        let action_label = if error.is_some() {
            Some(l.text("重试", "Retry"))
        } else if paused || pending {
            Some(l.text("继续检查", "Check now"))
        } else {
            None
        };
        egui::Frame::new()
            .fill(SURFACE)
            .stroke(Stroke::new(1.0, design::LINE))
            .corner_radius(8)
            .inner_margin(egui::Margin::symmetric(10, 6))
            .show(ui, |ui| {
                ui.set_min_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 8.0;
                    let (avatar, avatar_resp) =
                        ui.allocate_exact_size(Vec2::splat(24.0), Sense::click());
                    let texture = self.online_video.previews.texture(
                        ui,
                        avatar,
                        creator.uploader.avatar.as_deref(),
                        true,
                    );
                    design::initial_avatar(ui, avatar, texture.as_ref(), &creator.uploader.uname);
                    if avatar_resp
                        .on_hover_cursor(egui::CursorIcon::PointingHand)
                        .on_hover_text(l.text("查看投稿", "View uploads"))
                        .clicked()
                    {
                        self.open_creator_from_updates(creator);
                    }
                    let action_w = action_label
                        .map(|text| {
                            ui.painter()
                                .layout_no_wrap(
                                    text.into(),
                                    egui::TextStyle::Button.resolve(ui.style()),
                                    design::TEXT,
                                )
                                .size()
                                .x
                                + 28.0
                        })
                        .unwrap_or(0.0)
                        + 38.0;
                    let left_w = (ui.available_width() - action_w).max(80.0);
                    ui.allocate_ui_with_layout(
                        vec2(left_w, design::COMPACT_CONTROL_HEIGHT),
                        Layout::left_to_right(Align::Center),
                        |ui| {
                            ui.spacing_mut().item_spacing.x = 8.0;
                            let name_resp = ui.add(
                                egui::Label::new(
                                    RichText::new(&creator.uploader.uname).color(design::TEXT),
                                )
                                .sense(Sense::click())
                                .truncate(),
                            );
                            if name_resp
                                .on_hover_cursor(egui::CursorIcon::PointingHand)
                                .on_hover_text(&creator.uploader.uname)
                                .clicked()
                            {
                                self.open_creator_from_updates(creator);
                            }
                            ui.add(
                                egui::Label::new(
                                    RichText::new(&status_text).small().color(if is_error {
                                        Color32::LIGHT_RED
                                    } else {
                                        MUTED
                                    }),
                                )
                                .truncate(),
                            )
                            .on_hover_text(&status_text);
                        },
                    );
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.spacing_mut().item_spacing.x = 6.0;
                        let more = design::compact_icon_button(
                            ui,
                            icons::DOTS_THREE,
                            l.text("更多", "More"),
                            false,
                        );
                        egui::Popup::menu(&more)
                            .show(|ui| self.update_creator_menu(ui, creator, &[]));
                        if let Some(action) = action_label
                            && design::compact_control(ui, action, None, design::Control::Outline)
                                .clicked()
                        {
                            self.queue_video_updates(true, Some(id));
                        }
                    });
                });
            });
    }

    pub(in crate::app) fn video_updates_page(&mut self, ui: &mut Ui) {
        let l = self.settings.language;
        if self.online_video.updates.undo_available() {
            let (undo, _) = design::floating_toast(
                ui.ctx(),
                Id::new("video-updates-undo"),
                l.text("已标为已看", "Marked as seen"),
                Some(l.text("撤销标为已看", "Undo mark as seen")),
                None,
            );
            if undo {
                self.online_video.updates.undo();
            }
            ui.ctx().request_repaint_after(Duration::from_secs(1));
        }
        if self.settings.sync.enabled && self.cloud.failed {
            design::alert_banner(
                ui,
                true,
                &format!(
                    "{} · {}",
                    l.text("云同步暂不可用", "Cloud sync unavailable"),
                    self.cloud.status
                ),
                None,
                None,
            );
            ui.add_space(10.0);
        }
        if !self.online_video.updates.status.is_empty() {
            let status = self.online_video.updates.status.clone();
            let is_warning = !self.online_video.updates.stopped;
            let (retry, dismiss) = design::alert_banner(
                ui,
                is_warning,
                &status,
                Some(l.text("重试", "Retry")),
                Some(l.text("关闭", "Dismiss")),
            );
            if retry {
                self.queue_video_updates(true, None);
            } else if dismiss {
                self.online_video.updates.status.clear();
                self.online_video.updates.stopped = false;
            }
            ui.add_space(10.0);
        }
        let mut creators: Vec<_> = self
            .library
            .creators
            .iter()
            .filter(|r| r.favorite)
            .cloned()
            .collect();
        creators.sort_by_key(|r| {
            let posts = self
                .online_video
                .updates
                .content
                .get(&key(r))
                .map(|c| c.unread())
                .unwrap_or_default();
            (
                posts.is_empty(),
                std::cmp::Reverse(posts.iter().filter_map(|p| p.published).max().unwrap_or(0)),
                std::cmp::Reverse(r.favorited_at),
                r.uploader.mid,
            )
        });
        if creators.is_empty() {
            design::empty_state_with(
                ui,
                icons::FILM_SLATE,
                l.text(
                    "收藏 UP 主，及时查看新投稿",
                    "Favorite creators to see their new uploads",
                ),
                |ui| {
                    if design::control(
                        ui,
                        l.text("搜索 UP 主", "Find creators"),
                        Some(draw_search),
                        design::Control::Outline,
                        0.0,
                    )
                    .clicked()
                    {
                        self.online_video.kind = SearchKind::Uploader;
                        self.show_online_video();
                    }
                },
            );
            return;
        }
        let mut updated_groups = Vec::new();
        let mut status_rows = Vec::new();
        for creator in &creators {
            let id = key(creator);
            let posts = self
                .online_video
                .updates
                .content
                .get(&id)
                .map(|c| c.unread())
                .unwrap_or_default();
            let local = self.online_video.updates.local.get(&id);
            let error = local.and_then(|l| l.error.clone());
            let paused = local.is_some_and(|l| l.paused);
            let pending = self
                .online_video
                .updates
                .content
                .get(&id)
                .is_none_or(|c| c.baseline.is_none());
            if !posts.is_empty() {
                updated_groups.push((creator.clone(), id, posts, error, paused));
            } else if error.is_some() || paused || pending {
                status_rows.push((creator.clone(), id, error, pending, paused));
            }
        }
        design::scroll()
            .id_salt("video-updates")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for (creator, id, posts, error, paused) in &updated_groups {
                    ui.push_id(id, |ui| {
                        let collapsed = self.online_video.updates.collapsed.contains(id);
                        let ids: Vec<_> = posts.iter().map(|p| p.bvid.clone()).collect();
                        self.update_group_header(ui, creator, id, &ids, collapsed);
                        if let Some(err) = error {
                            ui.add_space(6.0);
                            let (retry, _) = design::alert_banner(
                                ui,
                                false,
                                err,
                                Some(l.text("重试", "Retry")),
                                None,
                            );
                            if retry {
                                self.queue_video_updates(true, Some(id));
                            }
                        } else if *paused {
                            ui.add_space(6.0);
                            let (resume, _) = design::alert_banner(
                                ui,
                                true,
                                l.text("仍有投稿待补查", "More uploads remain to check"),
                                Some(l.text("继续检查", "Continue checking")),
                                None,
                            );
                            if resume {
                                self.queue_video_updates(true, Some(id));
                            }
                        }
                        if !collapsed {
                            ui.add_space(8.0);
                            let expanded = self.online_video.updates.expanded.contains(id);
                            let visible = if expanded {
                                posts.len()
                            } else {
                                posts.len().min(6)
                            };
                            let columns = cards::result_columns(ui.available_width());
                            let width = cards::card_width(ui.available_width(), columns);
                            let now = chrono::Local::now();
                            for row in posts[..visible].chunks(columns) {
                                ui.horizontal_top(|ui| {
                                    ui.spacing_mut().item_spacing.x = 12.0;
                                    for post in row {
                                        ui.push_id(&post.bvid, |ui| {
                                            self.update_post_card(ui, id, post, width, &now);
                                        });
                                    }
                                });
                                ui.add_space(4.0);
                            }
                            if posts.len() > 6 {
                                ui.add_space(4.0);
                                if design::compact_control(
                                    ui,
                                    &if expanded {
                                        l.text("收起", "Show less").into()
                                    } else {
                                        format!(
                                            "{} {} {}",
                                            l.text("展开其余", "Show remaining"),
                                            posts.len() - 6,
                                            l.text("个", "videos")
                                        )
                                    },
                                    None,
                                    design::Control::Outline,
                                )
                                .clicked()
                                {
                                    if expanded {
                                        self.online_video.updates.expanded.remove(id);
                                    } else {
                                        self.online_video.updates.expanded.insert(id.clone());
                                    }
                                }
                            }
                        }
                        ui.add_space(18.0);
                    });
                }
                if !status_rows.is_empty() {
                    if !updated_groups.is_empty() {
                        ui.label(
                            RichText::new(l.text("异常与待检查", "Checks needing attention"))
                                .small()
                                .color(MUTED),
                        );
                        ui.add_space(6.0);
                    }
                    for (creator, id, error, pending, paused) in &status_rows {
                        ui.push_id(("compact-status", id), |ui| {
                            self.update_compact_row(
                                ui,
                                creator,
                                id,
                                error.as_deref(),
                                *pending,
                                *paused,
                            );
                        });
                        ui.add_space(6.0);
                    }
                }
                if updated_groups.is_empty() && status_rows.is_empty() {
                    let last_checked = self
                        .online_video
                        .updates
                        .local
                        .values()
                        .map(|l| l.success)
                        .filter(|t| *t > 0)
                        .max()
                        .and_then(|time| chrono::DateTime::from_timestamp(time as i64, 0))
                        .map(|date| {
                            format!(
                                "{} {}",
                                l.text("最近检查", "Last checked"),
                                date.with_timezone(&chrono::Local).format("%m-%d %H:%M")
                            )
                        });
                    design::empty_state_with(
                        ui,
                        icons::FILM_SLATE,
                        l.text("暂无新投稿", "No new uploads"),
                        |ui| {
                            if let Some(caption) = &last_checked {
                                ui.small(RichText::new(caption).color(MUTED));
                            }
                        },
                    );
                }
            });
    }
}
