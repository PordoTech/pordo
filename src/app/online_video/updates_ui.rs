use super::*;
use updates::key;

impl App {
    pub(in crate::app) fn video_library_tabs(&mut self, ui: &mut Ui) {
        let l = self.settings.language;
        let (creators, videos) = self.online_video.updates.counts(&self.library.creators);
        let label = if creators > 0 {
            format!("{} · {creators}", l.text("更新", "Updates"))
        } else {
            l.text("更新", "Updates").into()
        };
        let top = ui.cursor().min;
        let right = ui.max_rect().right();
        let previous = self.workspace.video_tab;
        ui.scope(|ui| {
            design::tabs(
                ui,
                &mut self.workspace.video_tab,
                &[
                    l.text("收藏", "Favorites"),
                    l.text("搜索", "Search"),
                    &label,
                ],
            );
        })
        .response
        .on_hover_text(format!(
            "{} {creators} · {} {videos}",
            l.text("有更新的 UP 主", "Creators with updates"),
            l.text("未看视频", "Unseen videos")
        ));
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
    fn update_creator_menu(
        &mut self,
        ui: &mut Ui,
        creator: &library::CreatorRecord,
        ids: &[String],
    ) {
        let l = self.settings.language;
        if design::menu_item(ui, false, l.text("查看投稿", "View uploads")).clicked() {
            self.online_video.updates.return_to_updates = true;
            self.open_bilibili_creator(creator.uploader.clone());
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
    pub(in crate::app) fn video_updates_page(&mut self, ui: &mut Ui) {
        let l = self.settings.language;
        if self.online_video.updates.undo_available() {
            if design::control(
                ui,
                l.text("撤销标为已看", "Undo mark as seen"),
                None,
                design::Control::Quiet,
                0.0,
            )
            .clicked()
            {
                self.online_video.updates.undo();
            }
            ui.ctx().request_repaint_after(Duration::from_secs(1));
        }
        if self.settings.sync.enabled && self.cloud.failed {
            ui.label(
                RichText::new(format!(
                    "{} · {}",
                    l.text("云同步暂不可用", "Cloud sync unavailable"),
                    self.cloud.status
                ))
                .small()
                .color(Color32::LIGHT_RED),
            );
        }
        if !self.online_video.updates.status.is_empty() {
            ui.label(&self.online_video.updates.status);
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
            design::empty_state(
                ui,
                icons::FILM_SLATE,
                l.text(
                    "收藏 UP 主，及时查看新投稿",
                    "Favorite creators to see their new uploads",
                ),
            );
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
            return;
        }
        let mut shown = false;
        design::scroll()
            .id_salt("video-updates")
            .auto_shrink([false, false])
            .show(ui, |ui| {
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
                    if posts.is_empty() && error.is_none() && !paused && !pending {
                        continue;
                    }
                    shown = true;
                    ui.push_id(&id, |ui| {
                        let collapsed = self.online_video.updates.collapsed.contains(&id);
                        let ids: Vec<_> = posts.iter().map(|p| p.bvid.clone()).collect();
                        ui.horizontal(|ui| {
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
                                    self.online_video.updates.collapsed.remove(&id);
                                } else {
                                    self.online_video.updates.collapsed.insert(id.clone());
                                }
                            }
                            let (avatar, response) =
                                ui.allocate_exact_size(Vec2::splat(32.0), Sense::click());
                            ui.painter().rect_filled(avatar, 16, SURFACE);
                            if let Some(texture) = self.online_video.previews.texture(
                                ui,
                                avatar,
                                creator.uploader.avatar.as_deref(),
                                true,
                            ) {
                                design::cover(ui, &texture, avatar, 16);
                            }
                            if response
                                .on_hover_text(l.text("查看投稿", "View uploads"))
                                .clicked()
                            {
                                self.online_video.updates.return_to_updates = true;
                                self.open_bilibili_creator(creator.uploader.clone());
                            }
                            let reserve = if ui.available_width() > 470.0 {
                                270.0
                            } else {
                                98.0
                            };
                            let name_width = (ui.available_width() - reserve).max(50.0);
                            ui.allocate_ui_with_layout(
                                vec2(name_width, 32.0),
                                Layout::left_to_right(Align::Center),
                                |ui| {
                                    ui.add(
                                        egui::Label::new(
                                            RichText::new(&creator.uploader.uname).strong(),
                                        )
                                        .truncate(),
                                    )
                                    .on_hover_text(&creator.uploader.uname);
                                },
                            );
                            ui.label(
                                RichText::new(format!(
                                    "{} {}",
                                    posts.len(),
                                    l.text("未看", "unseen")
                                ))
                                .small()
                                .color(MUTED),
                            );
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                let more = design::compact_icon_button(
                                    ui,
                                    icons::DOTS_THREE,
                                    l.text("更多", "More"),
                                    false,
                                );
                                egui::Popup::menu(&more)
                                    .show(|ui| self.update_creator_menu(ui, creator, &ids));
                                if ui.available_width()
                                    >= design::control_width(
                                        ui,
                                        l.text("全部标为已看", "Mark all as seen"),
                                        false,
                                    )
                                {
                                    ui.add_enabled_ui(!posts.is_empty(), |ui| {
                                        if design::control(
                                            ui,
                                            l.text("全部标为已看", "Mark all as seen"),
                                            None,
                                            design::Control::Quiet,
                                            0.0,
                                        )
                                        .clicked()
                                        {
                                            self.online_video.updates.mark_read(&id, ids.clone());
                                        }
                                    });
                                }
                            });
                        });
                        if let Some(error) = &error {
                            ui.horizontal_wrapped(|ui| {
                                ui.label(RichText::new(error).small().color(Color32::LIGHT_RED));
                                if design::control(
                                    ui,
                                    l.text("重试", "Retry"),
                                    None,
                                    design::Control::Quiet,
                                    0.0,
                                )
                                .clicked()
                                {
                                    self.queue_video_updates(true, Some(&id));
                                }
                            });
                        } else if pending {
                            let text = if !self.video_updates_cloud_ready() {
                                l.text(
                                    "等待首次云同步，恢复投稿基线",
                                    "Waiting for cloud sync to restore the baseline",
                                )
                            } else {
                                l.text("尚未完成首次检查", "First update check not completed")
                            };
                            ui.label(RichText::new(text).small().color(MUTED));
                        }
                        if paused {
                            ui.horizontal_wrapped(|ui| {
                                ui.label(
                                    RichText::new(
                                        l.text("仍有投稿待补查", "More uploads remain to check"),
                                    )
                                    .small()
                                    .color(MUTED),
                                );
                                if design::control(
                                    ui,
                                    l.text("继续检查", "Continue checking"),
                                    None,
                                    design::Control::Quiet,
                                    0.0,
                                )
                                .clicked()
                                {
                                    self.queue_video_updates(true, Some(&id));
                                }
                            });
                        }
                        if !collapsed && !posts.is_empty() {
                            let expanded = self.online_video.updates.expanded.contains(&id);
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
                                            let response = cards::video_card(
                                                ui,
                                                &mut self.online_video.previews,
                                                &post.video(),
                                                width,
                                                l,
                                                false,
                                                &now,
                                            );
                                            ui.painter().circle_filled(
                                                response.rect.left_top() + vec2(16.0, 16.0),
                                                3.0,
                                                ACCENT,
                                            );
                                            let menu_rect = Rect::from_min_size(
                                                response.rect.right_top() + vec2(-42.0, 12.0),
                                                Vec2::splat(30.0),
                                            );
                                            if response.hovered()
                                                || response.has_focus()
                                                || ui.rect_contains_pointer(menu_rect)
                                            {
                                                ui.painter().rect_filled(
                                                    menu_rect,
                                                    6,
                                                    Color32::from_black_alpha(170),
                                                );
                                                let mut buttons = workspace::child(
                                                    ui,
                                                    "video-update-menu",
                                                    menu_rect,
                                                );
                                                let button = design::compact_icon_button(
                                                    &mut buttons,
                                                    icons::DOTS_THREE,
                                                    l.text("更多", "More"),
                                                    false,
                                                );
                                                egui::Popup::menu(&button).show(|ui| {
                                                    if design::menu_item(
                                                        ui,
                                                        false,
                                                        l.text("标为已看", "Mark as seen"),
                                                    )
                                                    .clicked()
                                                    {
                                                        self.online_video.updates.mark_read(
                                                            &id,
                                                            vec![post.bvid.clone()],
                                                        );
                                                        ui.close();
                                                    }
                                                });
                                            }
                                            response.context_menu(|ui| {
                                                if design::menu_item(
                                                    ui,
                                                    false,
                                                    l.text("标为已看", "Mark as seen"),
                                                )
                                                .clicked()
                                                {
                                                    self.online_video
                                                        .updates
                                                        .mark_read(&id, vec![post.bvid.clone()]);
                                                    ui.close();
                                                }
                                            });
                                            if response.clicked() {
                                                self.open_update_video(&post.bvid, &post.title);
                                            }
                                        });
                                    }
                                });
                                ui.add_space(4.0);
                            }
                            if posts.len() > 6
                                && design::control(
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
                                    design::Control::Quiet,
                                    0.0,
                                )
                                .clicked()
                            {
                                if expanded {
                                    self.online_video.updates.expanded.remove(&id);
                                } else {
                                    self.online_video.updates.expanded.insert(id.clone());
                                }
                            }
                        }
                        ui.add_space(20.0);
                    });
                }
                if !shown {
                    design::empty_state(
                        ui,
                        icons::FILM_SLATE,
                        l.text("暂无新投稿", "No new uploads"),
                    );
                    if let Some(time) = self
                        .online_video
                        .updates
                        .local
                        .values()
                        .map(|l| l.success)
                        .filter(|t| *t > 0)
                        .max()
                        && let Some(date) = chrono::DateTime::from_timestamp(time as i64, 0)
                    {
                        ui.small(format!(
                            "{} {}",
                            l.text("最近检查", "Last checked"),
                            date.with_timezone(&chrono::Local).format("%m-%d %H:%M")
                        ));
                    }
                }
            });
    }
}
