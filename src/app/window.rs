use super::*;

pub(super) const CONTROLS_WIDTH: f32 = 138.0;
pub(super) const CAPTION_HEIGHT: f32 = 44.0;

#[derive(Clone, Default)]
struct FullscreenState {
    requested: Option<bool>,
    restore_maximized: bool,
}

pub(super) fn fullscreen(ctx: &egui::Context) -> bool {
    let actual = ctx.input(|i| i.viewport().fullscreen);
    ctx.data_mut(|data| {
        let id = Id::new("window-fullscreen-state");
        let mut state = data.get_temp::<FullscreenState>(id).unwrap_or_default();
        if actual.is_some() && state.requested == actual {
            state.requested = None;
        }
        let active = state.requested.or(actual).unwrap_or(false);
        data.insert_temp(id, state);
        active
    })
}

pub(super) fn set_fullscreen(ctx: &egui::Context, enabled: bool) {
    if fullscreen(ctx) == enabled {
        return;
    }
    let id = Id::new("window-fullscreen-state");
    let mut state = ctx
        .data_mut(|d| d.get_temp::<FullscreenState>(id))
        .unwrap_or_default();
    if enabled {
        state.restore_maximized = ctx.input(|i| i.viewport().maximized.unwrap_or(false));
        if state.restore_maximized {
            // winit 0.30 on Windows otherwise retains WS_MAXIMIZE in fullscreen,
            // and WM_NCCALCSIZE restricts the client to rcWork (above the taskbar).
            ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(false));
        }
    }
    ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(enabled));
    // Refresh the Windows undecorated-shadow flag after changing fullscreen.
    ctx.send_viewport_cmd(egui::ViewportCommand::Decorations(false));
    if !enabled && state.restore_maximized {
        ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(true));
        state.restore_maximized = false;
    }
    state.requested = Some(enabled);
    ctx.data_mut(|d| d.insert_temp(id, state));
    ctx.request_repaint();
}

pub(super) fn fullscreen_shortcuts(ctx: &egui::Context) -> bool {
    let fresh_f11 = ctx.input(|i| {
        i.events.iter().any(|event| {
            matches!(event,
        egui::Event::Key { key: Key::F11, pressed: true, repeat: false, modifiers, .. }
            if modifiers.is_none())
        })
    });
    if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, Key::F11)) && fresh_f11 {
        set_fullscreen(ctx, !fullscreen(ctx));
    }
    // Consume Escape before page/dialog handlers so one press only exits fullscreen.
    if fullscreen(ctx) && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, Key::Escape)) {
        set_fullscreen(ctx, false);
    }
    fullscreen(ctx)
}

pub(super) fn controls_width(ctx: &egui::Context) -> f32 {
    if fullscreen(ctx) { 0.0 } else { CONTROLS_WIDTH }
}

pub(super) fn video_cursor(ctx: &egui::Context, may_hide: bool) {
    let (now, active) = ctx.input(|i| {
        (
            i.time,
            !i.focused
                || i.pointer.any_down()
                || i.events
                    .iter()
                    .any(|e| matches!(e, egui::Event::PointerMoved(_) | egui::Event::Key { .. })),
        )
    });
    let id = Id::new("fullscreen-video-pointer-activity");
    let last = ctx.data_mut(|d| {
        let mut last = d.get_temp::<f64>(id).unwrap_or(now);
        if active || !may_hide {
            last = now;
        }
        d.insert_temp(id, last);
        last
    });
    if may_hide && now - last >= 2.0 {
        ctx.set_cursor_icon(egui::CursorIcon::None);
    }
}

#[derive(Clone)]
struct Gesture {
    outer: Rect,
    pointer: egui::Pos2,
}

// Caption movement keeps its existing egui gesture; edge resizing is delegated
// to the OS and must never feed outer dimensions into client-size requests.
fn caption_gesture(ui: &Ui, response: &Response) {
    let id = response.id.with("window-gesture");
    let ctx = ui.ctx();
    let (outer, maximized, pointer, down) = ctx.input(|i| {
        (
            i.viewport().outer_rect,
            i.viewport().maximized.unwrap_or(false),
            i.pointer.interact_pos(),
            i.pointer.primary_down(),
        )
    });
    if maximized {
        if response.dragged() {
            ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(false));
        }
        return;
    }
    let mut gesture = ctx.data_mut(|d| d.get_temp::<Gesture>(id));
    if gesture.is_none()
        && response.is_pointer_button_down_on()
        && let (Some(outer), Some(pointer)) = (outer, pointer)
    {
        gesture = Some(Gesture {
            outer,
            pointer: outer.min + pointer.to_vec2(),
        });
    }
    if let Some(gesture) = gesture {
        if (response.dragged() || response.drag_stopped())
            && let (Some(outer), Some(pointer)) = (outer, pointer)
        {
            let delta = outer.min + pointer.to_vec2() - gesture.pointer;
            let target = gesture.outer.translate(delta);
            ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(target.min));
        }
        ctx.data_mut(|d| {
            if response.drag_stopped() || !down {
                d.remove::<Gesture>(id);
            } else {
                d.insert_temp(id, gesture);
            }
        });
    }
}

fn begin_resize(ui: &Ui, response: &Response, direction: egui::ResizeDirection) {
    let ctx = ui.ctx();
    // BeginResize must run immediately after a real press, once per input
    // frame. Held buttons, redraws and egui's extra layout passes must not
    // restart Windows' modal resize loop after capture changes or release.
    if !ui.is_enabled()
        || !response.is_pointer_button_down_on()
        || !ctx.input(|i| i.focused && i.pointer.primary_pressed())
        || fullscreen(ctx)
        || ctx.input(|i| i.viewport().maximized.unwrap_or(false))
    {
        return;
    }
    let frame = ctx.cumulative_frame_nr();
    let fresh = ctx.data_mut(|data| {
        let id = Id::new("window-native-resize-frame");
        if data.get_temp::<u64>(id) == Some(frame) {
            false
        } else {
            data.insert_temp(id, frame);
            true
        }
    });
    if fresh {
        ctx.send_viewport_cmd(egui::ViewportCommand::BeginResize(direction));
    }
}

/// Register the caption background before interactive content. Search fields,
/// navigation and caption buttons drawn afterwards retain their own hit areas.
pub(super) fn drag_region(ui: &mut Ui) {
    if fullscreen(ui.ctx()) {
        return;
    }
    let screen = ui.max_rect();
    let drag = Rect::from_min_max(
        screen.min + vec2(5.0, 5.0),
        pos2(
            screen.max.x - CONTROLS_WIDTH,
            screen.min.y + CAPTION_HEIGHT.min(screen.height()),
        ),
    );
    let response = ui.interact(drag, Id::new("window-drag-region"), Sense::click_and_drag());
    if response.double_clicked() {
        let maximized = ui.ctx().input(|i| i.viewport().maximized.unwrap_or(false));
        ui.ctx()
            .send_viewport_cmd(egui::ViewportCommand::Maximized(!maximized));
    } else {
        caption_gesture(ui, &response);
    }
}

impl App {
    pub(super) fn window_frame(&mut self, root: &mut Ui, show_controls: bool) {
        let screen = root.max_rect();
        let ctx = root.ctx().clone();
        let fullscreen = fullscreen(&ctx);
        let maximized = ctx.input(|i| i.viewport().maximized.unwrap_or(false));
        let reader = self.workspace.page == workspace::Page::Comic && self.comic.reading();
        let caption_height = if reader { 48.0 } else { CAPTION_HEIGHT };
        if show_controls && !fullscreen {
            let mut draw_controls = |root: &mut Ui| {
                let l = self.settings.language;
                let controls = Rect::from_min_size(
                    pos2(screen.max.x - CONTROLS_WIDTH, screen.min.y),
                    vec2(CONTROLS_WIDTH, caption_height),
                );
                for index in 0..3 {
                    let rect = Rect::from_min_size(
                        controls.min + vec2(index as f32 * 46.0, 0.0),
                        vec2(46.0, caption_height),
                    );
                    let response =
                        root.interact(rect, Id::new(("window-button", index)), Sense::click());
                    let label = match index {
                        0 => l.text("最小化", "Minimize"),
                        1 if maximized => l.text("还原窗口", "Restore window"),
                        1 => l.text("最大化", "Maximize"),
                        _ => l.text("关闭窗口", "Close window"),
                    };
                    response.widget_info(|| {
                        egui::WidgetInfo::labeled(egui::WidgetType::Button, true, label)
                    });
                    if response.hovered() {
                        root.painter().rect_filled(
                            rect,
                            0,
                            if index == 2 {
                                Color32::from_rgb(182, 55, 65)
                            } else {
                                design::SELECTED
                            },
                        );
                    }
                    let center = rect.center();
                    let stroke = Stroke::new(1.2, design::TEXT);
                    match index {
                        0 => {
                            root.painter().line_segment(
                                [center + vec2(-5.0, 0.0), center + vec2(5.0, 0.0)],
                                stroke,
                            );
                        }
                        1 if maximized => {
                            let back =
                                Rect::from_center_size(center + vec2(2.0, -2.0), Vec2::splat(8.0));
                            root.painter()
                                .line_segment([back.left_top(), back.right_top()], stroke);
                            root.painter()
                                .line_segment([back.right_top(), back.right_bottom()], stroke);
                            root.painter().rect_stroke(
                                Rect::from_center_size(center + vec2(-1.0, 1.0), Vec2::splat(8.0)),
                                0,
                                stroke,
                                egui::StrokeKind::Inside,
                            );
                        }
                        1 => {
                            root.painter().rect_stroke(
                                Rect::from_center_size(center, Vec2::splat(10.0)),
                                0,
                                stroke,
                                egui::StrokeKind::Inside,
                            );
                        }
                        _ => {
                            root.painter().line_segment(
                                [center + vec2(-5.0, -5.0), center + vec2(5.0, 5.0)],
                                stroke,
                            );
                            root.painter().line_segment(
                                [center + vec2(5.0, -5.0), center + vec2(-5.0, 5.0)],
                                stroke,
                            );
                        }
                    }
                    if response.on_hover_text(label).clicked() {
                        match index {
                            0 => ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true)),
                            1 => {
                                ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(!maximized))
                            }
                            _ => {
                                self.request_cloud_close(&ctx);
                            }
                        }
                    }
                }
                if reader {
                    caption_resize_edges(root, screen);
                }
            };
            if reader {
                egui::Area::new(Id::new("reader-window-controls"))
                    .order(egui::Order::Foreground)
                    .fixed_pos(pos2(screen.max.x - CONTROLS_WIDTH, screen.min.y))
                    .default_size(vec2(CONTROLS_WIDTH, caption_height))
                    .constrain(false)
                    .movable(false)
                    .sense(Sense::hover())
                    .show(&ctx, |ui| {
                        ui.set_clip_rect(Rect::from_min_size(
                            pos2(screen.max.x - CONTROLS_WIDTH, screen.min.y),
                            vec2(CONTROLS_WIDTH, caption_height),
                        ));
                        ui.set_min_size(vec2(CONTROLS_WIDTH, caption_height));
                        draw_controls(ui);
                    });
            } else {
                draw_controls(root);
            }
        }
        if !fullscreen && !maximized {
            root.painter().rect_stroke(
                screen,
                0,
                Stroke::new(1.0, design::LINE),
                egui::StrokeKind::Inside,
            );
            resize_edges(root);
        }
    }
}

fn resize_edges(ui: &mut Ui) {
    resize_edges_on(ui, ui.max_rect());
}

/// Overlay captions need resize handles in their own input layer too.
pub(super) fn caption_resize_edges(ui: &mut Ui, screen: Rect) {
    if !fullscreen(ui.ctx()) && !ui.ctx().input(|i| i.viewport().maximized.unwrap_or(false)) {
        resize_edges_on(ui, screen);
    }
}

fn resize_edges_on(ui: &mut Ui, screen: Rect) {
    use egui::{CursorIcon as C, ResizeDirection as D};
    let edge = 5.0;
    let corner = 12.0;
    let edges = [
        (
            D::NorthWest,
            C::ResizeNwSe,
            Rect::from_min_size(screen.min, Vec2::splat(corner)),
        ),
        (
            D::NorthEast,
            C::ResizeNeSw,
            Rect::from_min_size(screen.right_top() - vec2(corner, 0.0), Vec2::splat(corner)),
        ),
        (
            D::SouthWest,
            C::ResizeNeSw,
            Rect::from_min_size(
                screen.left_bottom() - vec2(0.0, corner),
                Vec2::splat(corner),
            ),
        ),
        (
            D::SouthEast,
            C::ResizeNwSe,
            Rect::from_min_size(screen.max - Vec2::splat(corner), Vec2::splat(corner)),
        ),
        (
            D::North,
            C::ResizeVertical,
            Rect::from_min_max(
                screen.min + vec2(corner, 0.0),
                screen.right_top() + vec2(-corner, edge),
            ),
        ),
        (
            D::South,
            C::ResizeVertical,
            Rect::from_min_max(
                screen.left_bottom() + vec2(corner, -edge),
                screen.max - vec2(corner, 0.0),
            ),
        ),
        (
            D::West,
            C::ResizeHorizontal,
            Rect::from_min_max(
                screen.min + vec2(0.0, corner),
                screen.left_bottom() + vec2(edge, -corner),
            ),
        ),
        (
            D::East,
            C::ResizeHorizontal,
            Rect::from_min_max(
                screen.right_top() + vec2(-edge, corner),
                screen.max - vec2(0.0, corner),
            ),
        ),
    ];
    for (direction, cursor, rect) in edges {
        let rect = rect.intersect(ui.clip_rect());
        if !rect.is_positive() {
            continue;
        }
        let response = ui
            .interact(
                rect,
                // The reader caption and window frame expose the same edge
                // in separate layers; egui requires a distinct ID per layer.
                Id::new(("window-resize", ui.layer_id(), direction as u8)),
                Sense::drag(),
            )
            .on_hover_cursor(cursor);
        begin_resize(ui, &response, direction);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(full: bool, maximized: bool, events: Vec<egui::Event>) -> egui::RawInput {
        let mut input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, vec2(1280.0, 800.0))),
            events,
            ..Default::default()
        };
        let viewport = input.viewports.get_mut(&egui::ViewportId::ROOT).unwrap();
        viewport.fullscreen = Some(full);
        viewport.maximized = Some(maximized);
        input
    }

    fn key(key: Key, repeat: bool) -> egui::Event {
        egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat,
            modifiers: egui::Modifiers::NONE,
        }
    }

    fn transition_commands(output: &mut egui::FullOutput) -> Vec<egui::ViewportCommand> {
        output.textures_delta.clear();
        output.viewport_output[&egui::ViewportId::ROOT]
            .commands
            .iter()
            .filter(|c| {
                matches!(
                    c,
                    egui::ViewportCommand::Maximized(_)
                        | egui::ViewportCommand::Fullscreen(_)
                        | egui::ViewportCommand::Decorations(_)
                )
            })
            .cloned()
            .collect()
    }

    #[test]
    fn fullscreen_clears_maximization_before_entering_and_restores_it_after_exit() {
        for maximized in [false, true] {
            let ctx = egui::Context::default();
            let mut enter = ctx.run_ui(input(false, maximized, vec![]), |ui| {
                set_fullscreen(ui.ctx(), true);
            });
            let mut expected = vec![];
            if maximized {
                expected.push(egui::ViewportCommand::Maximized(false));
            }
            expected.extend([
                egui::ViewportCommand::Fullscreen(true),
                egui::ViewportCommand::Decorations(false),
            ]);
            assert_eq!(transition_commands(&mut enter), expected);
            let mut pending = ctx.run_ui(input(false, false, vec![]), |ui| {
                assert!(
                    fullscreen(ui.ctx()),
                    "old native state must not undo an in-flight request"
                );
                assert_eq!(controls_width(ui.ctx()), 0.0);
            });
            pending.textures_delta.clear();
            let mut exit = ctx.run_ui(input(true, false, vec![key(Key::Escape, false)]), |ui| {
                assert!(!fullscreen_shortcuts(ui.ctx()));
                assert!(
                    !ui.input(|i| i.key_pressed(Key::Escape)),
                    "Escape must not also navigate the page"
                );
            });
            let mut expected = vec![
                egui::ViewportCommand::Fullscreen(false),
                egui::ViewportCommand::Decorations(false),
            ];
            if maximized {
                expected.push(egui::ViewportCommand::Maximized(true));
            }
            assert_eq!(transition_commands(&mut exit), expected);
        }
    }

    #[test]
    fn f11_does_not_repeatedly_toggle_while_held_and_keeps_restore_state() {
        let ctx = egui::Context::default();
        let mut output = ctx.run_ui(input(false, true, vec![key(Key::F11, false)]), |ui| {
            assert!(fullscreen_shortcuts(ui.ctx()));
        });
        output.textures_delta.clear();
        let mut output = ctx.run_ui(input(true, false, vec![key(Key::F11, true)]), |ui| {
            assert!(fullscreen_shortcuts(ui.ctx()));
        });
        assert!(
            output.viewport_output[&egui::ViewportId::ROOT]
                .commands
                .is_empty()
        );
        output.textures_delta.clear();
        let mut released = key(Key::F11, false);
        if let egui::Event::Key { pressed, .. } = &mut released {
            *pressed = false;
        }
        let mut output = ctx.run_ui(input(true, false, vec![released]), |_| {});
        output.textures_delta.clear();
        let mut output = ctx.run_ui(input(true, false, vec![key(Key::F11, false)]), |ui| {
            assert!(!fullscreen_shortcuts(ui.ctx()));
        });
        assert!(
            output.viewport_output[&egui::ViewportId::ROOT]
                .commands
                .contains(&egui::ViewportCommand::Maximized(true))
        );
        output.textures_delta.clear();
    }

    #[test]
    fn fullscreen_video_cursor_hides_only_when_idle_and_returns_on_activity() {
        let ctx = egui::Context::default();
        for (time, events, allowed, hidden) in [
            (
                0.0,
                vec![egui::Event::PointerMoved(pos2(100.0, 100.0))],
                true,
                false,
            ),
            (2.1, vec![], true, true),
            (
                2.2,
                vec![egui::Event::PointerMoved(pos2(101.0, 100.0))],
                true,
                false,
            ),
            (4.3, vec![], false, false),
            (4.4, vec![], true, false),
        ] {
            let mut raw = input(true, false, events);
            raw.time = Some(time);
            let mut output = ctx.run_ui(raw, |ui| video_cursor(ui.ctx(), allowed));
            output.textures_delta.clear();
            assert_eq!(
                output.platform_output.cursor_icon == egui::CursorIcon::None,
                hidden
            );
        }
    }

    #[test]
    fn reader_overlay_and_window_frame_resize_handles_coexist() {
        let ctx = egui::Context::default();
        let screen = Rect::from_min_size(egui::Pos2::ZERO, vec2(1280.0, 800.0));
        for visible in [false, true, true, false, true] {
            let mut output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(screen),
                    ..Default::default()
                },
                |ui| {
                    if visible {
                        egui::Area::new(Id::new("test-reader-header"))
                            .order(egui::Order::Middle)
                            .fixed_pos(screen.min)
                            .show(&ctx, |ui| {
                                ui.set_clip_rect(Rect::from_min_size(
                                    screen.min,
                                    vec2(screen.width(), 48.0),
                                ));
                                ui.set_min_size(vec2(screen.width(), 48.0));
                                caption_resize_edges(ui, screen);
                            });
                        egui::Area::new(Id::new("test-reader-window-controls"))
                            .order(egui::Order::Foreground)
                            .fixed_pos(pos2(screen.max.x - CONTROLS_WIDTH, screen.min.y))
                            .show(&ctx, |ui| {
                                ui.set_clip_rect(Rect::from_min_size(
                                    pos2(screen.max.x - CONTROLS_WIDTH, screen.min.y),
                                    vec2(CONTROLS_WIDTH, 48.0),
                                ));
                                ui.set_min_size(vec2(CONTROLS_WIDTH, 48.0));
                                caption_resize_edges(ui, screen);
                            });
                    }
                    resize_edges(ui);
                },
            );
            output.textures_delta.clear();
        }
    }

    #[test]
    fn native_resize_runs_once_without_size_feedback_after_redraw_release_or_focus_loss() {
        use egui::{ResizeDirection as D, ViewportCommand as V};
        for scale in [1.0, 1.25, 1.5, 2.0] {
            for (point, direction) in [
                (pos2(2.0, 2.0), D::NorthWest),
                (pos2(1278.0, 2.0), D::NorthEast),
                (pos2(2.0, 798.0), D::SouthWest),
                (pos2(1278.0, 798.0), D::SouthEast),
                (pos2(600.0, 2.0), D::North),
                (pos2(600.0, 798.0), D::South),
                (pos2(2.0, 400.0), D::West),
                (pos2(1278.0, 400.0), D::East),
            ] {
                let ctx = egui::Context::default();
                let button = |pressed| egui::Event::PointerButton {
                    pos: point,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                };
                let render = |events, growth: f32, focused, discard| {
                    let mut raw = input(false, false, events);
                    raw.focused = focused;
                    let viewport = raw.viewports.get_mut(&egui::ViewportId::ROOT).unwrap();
                    viewport.native_pixels_per_point = Some(scale);
                    viewport.inner_rect = Some(Rect::from_min_size(
                        pos2(208.0, 108.0),
                        vec2(1280.0 + growth, 800.0 + growth),
                    ));
                    viewport.outer_rect = Some(Rect::from_min_size(
                        pos2(200.0, 100.0),
                        vec2(1296.0 + growth, 816.0 + growth),
                    ));
                    let mut output = ctx.run_ui(raw, |ui| {
                        resize_edges(ui);
                        if discard && ctx.current_pass_index() == 0 {
                            ctx.request_discard("resize layout pass regression");
                        }
                    });
                    output.textures_delta.clear();
                    output.viewport_output[&egui::ViewportId::ROOT]
                        .commands
                        .iter()
                        .filter(|command| {
                            matches!(
                                command,
                                V::BeginResize(_) | V::InnerSize(_) | V::OuterPosition(_)
                            )
                        })
                        .cloned()
                        .collect::<Vec<_>>()
                };
                assert!(
                    render(vec![egui::Event::PointerMoved(point)], 0.0, true, false).is_empty()
                );
                let commands = render(vec![button(true)], 0.0, true, true);
                assert_eq!(
                    commands,
                    vec![V::BeginResize(direction)],
                    "{direction:?} at {scale}x"
                );
                for growth in [40.0, 80.0, 80.0, 80.0] {
                    assert!(render(vec![], growth, true, false).is_empty());
                }
                assert!(render(vec![], 80.0, false, false).is_empty());
                assert!(render(vec![button(false)], 80.0, true, false).is_empty());
                assert!(render(vec![], 80.0, true, false).is_empty());
                assert_eq!(
                    render(
                        vec![egui::Event::PointerMoved(point), button(true)],
                        0.0,
                        true,
                        false
                    ),
                    vec![V::BeginResize(direction)]
                );
            }
        }
    }

    #[test]
    fn native_resize_is_disabled_in_fullscreen_maximized_and_unfocused_windows() {
        for (full, maximized, focused) in [
            (true, false, true),
            (false, true, true),
            (false, false, false),
        ] {
            let ctx = egui::Context::default();
            let point = pos2(1278.0, 600.0);
            for pressed in [false, true] {
                let mut raw = input(full, maximized, vec![egui::Event::PointerMoved(point)]);
                raw.focused = focused;
                if pressed {
                    raw.events.push(egui::Event::PointerButton {
                        pos: point,
                        button: egui::PointerButton::Primary,
                        pressed: true,
                        modifiers: egui::Modifiers::NONE,
                    });
                }
                let mut output = ctx.run_ui(raw, resize_edges);
                output.textures_delta.clear();
                assert!(
                    !output.viewport_output[&egui::ViewportId::ROOT]
                        .commands
                        .iter()
                        .any(|command| matches!(
                            command,
                            egui::ViewportCommand::BeginResize(_)
                                | egui::ViewportCommand::InnerSize(_)
                                | egui::ViewportCommand::OuterPosition(_)
                        ))
                );
            }
        }
    }

    fn drag_from_to(from: egui::Pos2, to: egui::Pos2) -> Vec<egui::ViewportCommand> {
        let ctx = egui::Context::default();
        let draw = |ui: &mut Ui| {
            drag_region(ui);
            let mut content = workspace::child(ui, "page-under-window-frame", ui.max_rect());
            content.label("Media content");
            resize_edges(ui);
        };
        let input = |events| {
            let mut input = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(1280.0, 800.0))),
                events,
                ..Default::default()
            };
            input
                .viewports
                .entry(egui::ViewportId::ROOT)
                .or_default()
                .outer_rect = Some(Rect::from_min_size(pos2(200.0, 100.0), vec2(1280.0, 800.0)));
            input
        };
        let mut commands = Vec::new();
        for events in [
            vec![],
            vec![
                egui::Event::PointerMoved(from),
                egui::Event::PointerButton {
                    pos: from,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::default(),
                },
            ],
        ] {
            let mut output = ctx.run_ui(input(events), draw);
            commands.extend(
                output
                    .viewport_output
                    .values()
                    .flat_map(|v| v.commands.iter().cloned()),
            );
            output.textures_delta.clear();
        }
        let mut output = ctx.run_ui(input(vec![egui::Event::PointerMoved(to)]), draw);
        commands.extend(
            output
                .viewport_output
                .values()
                .flat_map(|v| v.commands.iter().cloned())
                .collect::<Vec<_>>(),
        );
        output.textures_delta.clear();
        commands
    }

    #[test]
    fn caption_drag_moves_window_and_edges_resize_without_capturing_content() {
        assert!(
            drag_from_to(pos2(850.0, 20.0), pos2(750.0, 90.0))
                .contains(&egui::ViewportCommand::OuterPosition(pos2(100.0, 170.0)))
        );
        assert!(
            drag_from_to(pos2(1278.0, 600.0), pos2(1178.0, 600.0)).contains(
                &egui::ViewportCommand::BeginResize(egui::ResizeDirection::East)
            )
        );
        assert!(
            !drag_from_to(pos2(600.0, 400.0), pos2(650.0, 400.0))
                .iter()
                .any(|c| matches!(
                    c,
                    egui::ViewportCommand::OuterPosition(_)
                        | egui::ViewportCommand::InnerSize(_)
                        | egui::ViewportCommand::BeginResize(_)
                ))
        );
    }
}
