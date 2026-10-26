//! The disk drawn as a bar of partitions. In edit mode, dragging a partition moves it and
//! dragging its edges resizes it; the arrow keys do the same in 1 GiB steps (Shift resizes).
//!
//! The bar is the disk's usable region and nothing else is positional: adjacency on screen is
//! adjacency on the disk, and a gap is free space.

use bootshift_core::layout::{Extent, Guid, Layout, Partition, Target};
use eframe::egui::{
    self, Align2, Color32, CornerRadius, CursorIcon, EventFilter, Id, Key, Modifiers, Painter,
    Pos2, Rect, Response, Sense, Stroke, StrokeKind, Ui, Vec2,
};

use crate::editing::{self, KEY_STEP_BYTES};
use crate::format::{short_name, size};
use crate::theme::{self, Face, Weight};
use crate::widgets;

/// Partitions narrower than this share of the usable region are slivers.
const SLIVER_SHARE: f64 = 0.01;
const MIN_BLOCK: f32 = 6.0;
const MIN_SLIVER: f32 = 9.0;
pub const READ_ONLY_HEIGHT: f32 = 49.0;
pub const EDITABLE_HEIGHT: f32 = 65.0;
const CAPSULE_STRIP: f32 = 29.0;
const LEADERS: f32 = 8.0;
const CAPSULE_GAP: f32 = 6.0;
const CAPSULE_MAX: f32 = 210.0;
const HANDLE_MAX: f32 = 10.5;
/// A hovered block narrower than this grows to it, so a thin partition can be read.
const MAGNIFY_MIN: f32 = 108.0;
const MAGNIFY_LIFT: f32 = 6.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Drag {
    Move,
    Start,
    End,
}

#[derive(Clone, Debug)]
struct DragState {
    guid: Guid,
    kind: Drag,
    origin: Target,
    from: Extent,
    pointer_x: f32,
}

/// What the user did to the bar this frame.
#[derive(Default)]
pub struct Output {
    /// Every partition whose extent changed, after snapping and clamping. A plain move or shrink
    /// is one entry; resizing an edge into a neighbour pushes it, so there can be several.
    pub edits: Vec<(Guid, Extent)>,
    pub picked: Option<Guid>,
}

pub struct DiskBar<'a> {
    pub id: &'static str,
    pub layout: &'a Layout,
    pub target: &'a Target,
    pub selected: Option<Guid>,
    pub editable: bool,
}

impl DiskBar<'_> {
    pub fn show(self, ui: &mut Ui) -> Output {
        let mut output = Output::default();
        let width = ui.available_width();
        let height = if self.editable {
            EDITABLE_HEIGHT
        } else {
            READ_ONLY_HEIGHT
        };
        let slivers = self.slivers();
        let capsules_rect = (self.editable && !slivers.is_empty()).then(|| {
            let (rect, _) = ui.allocate_exact_size(Vec2::new(width, CAPSULE_STRIP), Sense::hover());
            rect
        });
        let (rect, _) = ui.allocate_exact_size(Vec2::new(width, height), Sense::hover());
        let inner = rect.shrink(1.0);
        let blocks = self.blocks(inner);

        self.paint_ground(ui.painter(), rect);
        let drag = ui.data(|d| d.get_temp::<DragState>(self.drag_id()));
        let responses = self.interact(ui, &blocks, inner.width(), &mut output, drag.as_ref());
        let painter = ui.painter().with_clip_rect(inner);
        for block in blocks.iter().filter(|b| !b.sliver) {
            self.paint_block(ui, &painter, block, &responses, drag.as_ref());
        }
        for block in blocks.iter().filter(|b| b.sliver) {
            self.paint_block(ui, &painter, block, &responses, drag.as_ref());
        }
        if let Some(selected) = blocks.iter().find(|b| Some(b.guid) == self.selected) {
            paint_selection(ui.painter(), selected);
        }
        let hovered_block = blocks
            .iter()
            .zip(&responses)
            .find(|(_, r)| {
                r.body.hovered()
                    || r.handles
                        .as_ref()
                        .is_some_and(|(s, e)| s.hovered() || e.hovered())
            })
            .map(|(b, _)| b.guid);
        let hovered_capsule =
            capsules_rect.and_then(|strip| self.capsules(ui, strip, &blocks, &mut output));
        if self.editable {
            self.scale(ui);
        }
        // A hovered block grows in place to a readable width, above its neighbours, so a thin
        // partition (or one reached through its capsule) can be seen without moving anything.
        if self.editable && drag.is_none() {
            if let Some(block) = hovered_block
                .or(hovered_capsule)
                .and_then(|guid| blocks.iter().find(|b| b.guid == guid))
            {
                self.magnify(ui, inner, block);
            }
        }
        output
    }

    fn drag_id(&self) -> Id {
        Id::unique((self.id, "drag"))
    }

    fn slivers(&self) -> Vec<&Partition> {
        self.layout
            .partitions
            .iter()
            .filter(|p| is_sliver(self.layout, self.extent_of(p)))
            .collect()
    }

    fn extent_of(&self, partition: &Partition) -> Extent {
        self.target
            .get(&partition.unique_guid)
            .copied()
            .unwrap_or(partition.extent)
    }

    fn blocks(&self, inner: Rect) -> Vec<Block> {
        let mut blocks: Vec<Block> = self
            .layout
            .partitions
            .iter()
            .map(|partition| {
                let extent = self.extent_of(partition);
                let sliver = is_sliver(self.layout, extent);
                let (left, width) = block_span(self.layout, extent, inner.width(), sliver);
                let rect = Rect::from_min_size(
                    Pos2::new(inner.left() + left, inner.top()),
                    Vec2::new(width, inner.height()),
                );
                Block {
                    guid: partition.unique_guid,
                    rect,
                    hit: rect,
                    sliver,
                    movable: self.editable && partition.content.can_move(),
                    locked: self.editable && !partition.content.can_move(),
                }
            })
            .collect();
        // A sliver sits above its neighbours, so their hit areas stop where it starts. That
        // keeps the sliver clickable while the tab order stays the disk's order.
        let sliver_rects: Vec<Rect> = blocks.iter().filter(|b| b.sliver).map(|b| b.rect).collect();
        for block in blocks.iter_mut().filter(|b| !b.sliver) {
            for sliver in &sliver_rects {
                block.hit = carve(block.hit, *sliver);
            }
        }
        blocks
    }

    fn paint_ground(&self, painter: &Painter, rect: Rect) {
        painter.rect_filled(rect, 3, theme::BAR_GROUND);
        let inner = rect.shrink(1.0);
        let hairline = Stroke::new(1.0, theme::alpha(Color32::WHITE, 0.022));
        let mut x = inner.left() + 0.5;
        while x < inner.right() {
            painter.vline(x, inner.y_range(), hairline);
            x += 7.0;
        }
        let border = if self.editable {
            Color32::from_rgb(0x32, 0x32, 0x3a)
        } else {
            theme::RULE
        };
        painter.rect_stroke(rect, 3, Stroke::new(1.0, border), StrokeKind::Inside);
    }

    fn interact(
        &self,
        ui: &mut Ui,
        blocks: &[Block],
        bar_width: f32,
        output: &mut Output,
        drag: Option<&DragState>,
    ) -> Vec<BlockResponse> {
        let sectors_per_point =
            self.layout.geometry.usable.len.0 as f64 / f64::from(bar_width.max(1.0));
        let mut responses = Vec::with_capacity(blocks.len());
        for block in blocks {
            let id = Id::unique((self.id, block.guid));
            let sense = if block.movable {
                Sense::click_and_drag()
            } else {
                Sense::CLICK
            };
            let body = ui.interact(block.hit, id, sense);
            let name = self
                .layout
                .partition(block.guid)
                .map(short_name)
                .unwrap_or_default();
            body.widget_info(|| {
                if block.movable {
                    egui::WidgetInfo::labeled(
                        egui::Role::Slider,
                        true,
                        format!("{name}: drag to move, drag edges to resize"),
                    )
                } else {
                    egui::WidgetInfo::labeled(
                        egui::Role::Button,
                        true,
                        format!("{name} ({})", self.id),
                    )
                }
            });
            if body.clicked() {
                output.picked = Some(block.guid);
            }
            let mut handles = None;
            if block.movable && !block.sliver {
                let width = handle_width(block.rect.width());
                let start =
                    Rect::from_min_size(block.rect.min, Vec2::new(width, block.rect.height()))
                        .intersect(block.hit);
                let end = Rect::from_min_max(
                    Pos2::new(block.rect.right() - width, block.rect.top()),
                    block.rect.max,
                )
                .intersect(block.hit);
                // Handles are for the pointer only; the keyboard resizes with Shift and the
                // arrows on the block itself, so the handles aren't extra tab stops.
                let start = ui.interact(start, id.with("start"), Sense::DRAG);
                let end = ui.interact(end, id.with("end"), Sense::DRAG);
                start.widget_info(|| {
                    egui::WidgetInfo::labeled(
                        egui::Role::Splitter,
                        true,
                        format!("Resize the start of {name}"),
                    )
                });
                end.widget_info(|| {
                    egui::WidgetInfo::labeled(
                        egui::Role::Splitter,
                        true,
                        format!("Resize the end of {name}"),
                    )
                });
                self.follow_drag(
                    ui,
                    &start,
                    block.guid,
                    Drag::Start,
                    sectors_per_point,
                    output,
                );
                self.follow_drag(ui, &end, block.guid, Drag::End, sectors_per_point, output);
                handles = Some((start, end));
            }
            if block.movable {
                self.follow_drag(ui, &body, block.guid, Drag::Move, sectors_per_point, output);
                self.keyboard(ui, &body, block.guid, output);
            }
            let cursor = match (block.movable, drag) {
                (_, Some(state)) if state.kind == Drag::Move => CursorIcon::Grabbing,
                (_, Some(_)) => CursorIcon::ResizeHorizontal,
                (true, None) => CursorIcon::Grab,
                (false, None) if block.locked => CursorIcon::NotAllowed,
                (false, None) => CursorIcon::Default,
            };
            let body = body.on_hover_cursor(cursor);
            let handles = handles.map(|(start, end)| {
                let resize = if drag.is_some_and(|d| d.kind == Drag::Move) {
                    CursorIcon::Grabbing
                } else {
                    CursorIcon::ResizeHorizontal
                };
                (start.on_hover_cursor(resize), end.on_hover_cursor(resize))
            });
            responses.push(BlockResponse { body, handles });
        }
        // While dragging, the whole bar keeps the drag's cursor so it doesn't flicker over a
        // neighbour.
        if let Some(state) = ui.data(|d| d.get_temp::<DragState>(self.drag_id())) {
            ui.ctx().set_cursor_icon(if state.kind == Drag::Move {
                CursorIcon::Grabbing
            } else {
                CursorIcon::ResizeHorizontal
            });
        }
        responses
    }

    fn follow_drag(
        &self,
        ui: &Ui,
        response: &Response,
        guid: Guid,
        kind: Drag,
        sectors_per_point: f64,
        output: &mut Output,
    ) {
        let pointer_x = ui.input(|i| i.pointer.interact_pos()).map(|p| p.x);
        if response.drag_started() {
            // Measure from where the button went down, not from where the drag threshold was
            // crossed, so the partition follows the pointer exactly.
            let origin_x = ui.input(|i| i.pointer.press_origin()).map(|p| p.x);
            if let (Some(pointer_x), Some(from)) = (origin_x.or(pointer_x), self.target.get(&guid))
            {
                let state = DragState {
                    guid,
                    kind,
                    origin: self.target.clone(),
                    from: *from,
                    pointer_x,
                };
                ui.data_mut(|d| d.insert_temp(self.drag_id(), state));
            }
        }
        let state = ui.data(|d| d.get_temp::<DragState>(self.drag_id()));
        if let (true, Some(state), Some(pointer_x)) = (response.dragged(), state, pointer_x) {
            if state.guid == guid && state.kind == kind {
                let delta = f64::from(pointer_x - state.pointer_x) * sectors_per_point;
                let edits: Vec<_> =
                    apply(self.layout, &state.origin, guid, kind, state.from, delta)
                        .into_iter()
                        .filter(|(g, e)| self.target.get(g) != Some(e))
                        .collect();
                if !edits.is_empty() {
                    output.edits = edits;
                }
            }
        }
        if response.drag_stopped() {
            ui.data_mut(|d| d.remove::<DragState>(self.drag_id()));
        }
    }

    fn keyboard(&self, ui: &mut Ui, response: &Response, guid: Guid, output: &mut Output) {
        if !response.has_focus() {
            return;
        }
        // egui only takes the arrow-key lock from a widget that was also focused last frame.
        // Without one more frame, the first arrow press after Tab would move focus instead.
        if response.gained_focus() {
            ui.ctx().request_repaint();
        }
        ui.memory_mut(|m| {
            m.set_focus_lock_filter(
                response.id,
                EventFilter {
                    horizontal_arrows: true,
                    ..EventFilter::default()
                },
            );
        });
        let Some(current) = self.target.get(&guid).copied() else {
            return;
        };
        let step = (KEY_STEP_BYTES / u64::from(self.layout.geometry.sector_size)) as f64;
        // Shift first: a plain-arrow match also accepts Shift, which would turn every resize
        // into a move.
        let pressed = ui.input_mut(|i| {
            [
                (Modifiers::SHIFT, Key::ArrowLeft, Drag::End, -step),
                (Modifiers::SHIFT, Key::ArrowRight, Drag::End, step),
                (Modifiers::NONE, Key::ArrowLeft, Drag::Move, -step),
                (Modifiers::NONE, Key::ArrowRight, Drag::Move, step),
            ]
            .into_iter()
            .find(|(modifiers, key, _, _)| i.consume_key(*modifiers, *key))
        });
        if let Some((_, _, kind, delta)) = pressed {
            let edits = apply(self.layout, self.target, guid, kind, current, delta);
            if !edits.is_empty() {
                output.edits = edits;
            }
        }
    }

    fn paint_block(
        &self,
        ui: &Ui,
        painter: &Painter,
        block: &Block,
        responses: &[BlockResponse],
        drag: Option<&DragState>,
    ) {
        let Some(partition) = self.layout.partition(block.guid) else {
            return;
        };
        let Some(response) = responses
            .iter()
            .find(|r| r.body.id == Id::unique((self.id, block.guid)))
        else {
            return;
        };
        let extent = self.extent_of(partition);
        let handle_hovered = response
            .handles
            .as_ref()
            .is_some_and(|(s, e)| s.hovered() || e.hovered());
        let hovered = block.movable && (response.body.hovered() || handle_hovered);
        let lift = |colour: Color32| {
            if hovered && drag.is_none() {
                brighten(colour)
            } else {
                colour
            }
        };
        let rect = block.rect;

        painter.rect_filled(rect, 0, lift(theme::block_colour(partition.content)));
        let used_share = (partition.used.0 as f32 / extent.len.0.max(1) as f32).min(1.0);
        if used_share > 0.0 {
            let used = Rect::from_min_size(
                rect.min,
                Vec2::new(rect.width() * used_share, rect.height()),
            );
            painter.rect_filled(used, 0, lift(theme::content_colour(partition.content)));
            hairlines(painter, used, 6.0, theme::alpha(theme::BAR_GROUND, 0.35));
        }
        if block.locked {
            hatch(painter, rect);
        }
        painter.vline(
            rect.right() - 0.5,
            rect.y_range(),
            Stroke::new(1.0, theme::BAR_GROUND),
        );

        if block.sliver {
            let ring = if hovered || Some(block.guid) == self.selected {
                theme::SIGNAL
            } else {
                theme::alpha(theme::BAR_GROUND, 0.85)
            };
            painter.rect_stroke(rect, 0, Stroke::new(1.0, ring), StrokeKind::Inside);
        } else {
            self.paint_label(ui, painter, partition, extent, rect);
            if block.movable {
                let focused = widgets::shows_focus(ui, &response.body);
                paint_grip(painter, rect, hovered || focused);
                if let Some((start, end)) = &response.handles {
                    let resizing =
                        drag.is_some_and(|d| d.guid == block.guid && d.kind != Drag::Move);
                    paint_handle(painter, start.rect, true, resizing || start.hovered());
                    paint_handle(painter, end.rect, false, resizing || end.hovered());
                }
            }
        }
        if widgets::shows_focus(ui, &response.body) {
            painter.rect_stroke(rect, 0, Stroke::new(2.0, theme::SIGNAL), StrokeKind::Inside);
        }
    }

    fn paint_label(
        &self,
        ui: &Ui,
        painter: &Painter,
        partition: &Partition,
        extent: Extent,
        rect: Rect,
    ) {
        let room = rect.width() - 16.0;
        if room <= 4.0 {
            return;
        }
        let name = short_name(partition);
        let top = Pos2::new(rect.left() + 8.0, rect.top() + 6.0);
        let shadow = theme::alpha(Color32::from_rgb(4, 4, 6), 0.7);
        let name_face = Face::Sans(Weight::SemiBold);
        let label = widgets::elided(ui, name.clone(), name_face, 11.0, theme::BLOCK_TEXT, room);
        let label_shadow = widgets::elided(ui, name, name_face, 11.0, shadow, room);
        painter.galley(top + Vec2::new(0.0, 1.0), label_shadow, shadow);
        let name_height = label.size().y;
        painter.galley(top, label, theme::BLOCK_TEXT);

        let size_text = size(self.layout, extent.len);
        let size_colour = theme::alpha(theme::BLOCK_TEXT, 0.88);
        let size_top = top + Vec2::new(0.0, name_height);
        let size_galley = widgets::elided(ui, size_text, theme::MONO, 10.0, size_colour, room);
        painter.galley(size_top, size_galley, size_colour);
    }

    fn capsules(
        &self,
        ui: &mut Ui,
        strip: Rect,
        blocks: &[Block],
        output: &mut Output,
    ) -> Option<Guid> {
        let mut hovered = None;
        let slivers: Vec<(&Partition, &Block)> = blocks
            .iter()
            .filter(|b| b.sliver)
            .filter_map(|b| self.layout.partition(b.guid).map(|p| (p, b)))
            .collect();
        let laid_out: Vec<Capsule> = slivers
            .iter()
            .map(|(partition, block)| self.capsule(ui, partition, block))
            .collect();
        let anchors: Vec<(f32, f32)> = laid_out
            .iter()
            .map(|c| (c.anchor - strip.left(), c.width))
            .collect();
        let lefts = place_capsules(&anchors, strip.width());
        let pill_height = strip.height() - LEADERS;
        let leader_top = strip.bottom() - LEADERS;
        for (capsule, left) in laid_out.into_iter().zip(lefts) {
            let rect = Rect::from_min_size(
                Pos2::new(strip.left() + left, strip.top()),
                Vec2::new(capsule.width, pill_height - 2.0),
            );
            let id = Id::unique((self.id, "capsule", capsule.guid));
            let sense = if capsule.pickable {
                Sense::click()
            } else {
                Sense::hover()
            };
            let response = ui.interact(rect, id, sense);
            response.widget_info(|| {
                egui::WidgetInfo::labeled(egui::Role::Button, capsule.pickable, &capsule.label)
            });
            let response = if capsule.pickable {
                response.on_hover_cursor(CursorIcon::PointingHand)
            } else {
                response
            };
            if response.hovered() {
                hovered = Some(capsule.guid);
            }
            if response.clicked() {
                ui.memory_mut(|m| m.request_focus(Id::unique((self.id, capsule.guid))));
                output.picked = Some(capsule.guid);
            }
            let painter = ui.painter();
            let border = if capsule.pickable && response.hovered() {
                theme::SIGNAL
            } else {
                theme::RULE
            };
            painter.rect_filled(rect, CornerRadius::same(127), theme::DECK_HIGH);
            painter.rect_stroke(
                rect,
                CornerRadius::same(127),
                Stroke::new(1.0, border),
                StrokeKind::Inside,
            );
            let mut x = rect.left() + 8.0;
            painter.circle_filled(Pos2::new(x + 3.0, rect.center().y), 3.0, capsule.dot);
            x += 6.0 + 5.0;
            let name = widgets::text_at(
                painter,
                Align2::LEFT_CENTER,
                Pos2::new(x, rect.center().y),
                capsule.name,
            );
            widgets::text_at(
                painter,
                Align2::LEFT_CENTER,
                Pos2::new(name.right() + 5.0, rect.center().y),
                capsule.size,
            );
            if widgets::shows_focus(ui, &response) {
                widgets::focus_ring(painter, rect, 9);
            }
            painter.line_segment(
                [
                    Pos2::new(rect.center().x, leader_top),
                    Pos2::new(capsule.anchor, strip.bottom()),
                ],
                Stroke::new(1.0, theme::LEADER),
            );
        }
        hovered
    }

    fn capsule(&self, ui: &Ui, partition: &Partition, block: &Block) -> Capsule {
        let extent = self.extent_of(partition);
        let size_galley = widgets::galley(
            ui,
            size(self.layout, extent.len),
            theme::MONO,
            9.6,
            theme::FAINT,
        );
        let fixed = 8.0 + 6.0 + 5.0 + 5.0 + size_galley.size().x + 8.0;
        let name_room = (CAPSULE_MAX - fixed).max(10.0);
        let name = widgets::elided(
            ui,
            short_name(partition),
            theme::MONO,
            9.6,
            theme::INK,
            name_room,
        );
        Capsule {
            guid: partition.unique_guid,
            label: format!(
                "{}, {}, {}",
                short_name(partition),
                partition.content.name(),
                size(self.layout, extent.len)
            ),
            anchor: block.rect.center().x,
            width: (fixed + name.size().x).min(CAPSULE_MAX).round(),
            name,
            size: size_galley,
            dot: theme::content_colour(partition.content),
            pickable: block.movable,
        }
    }

    /// Tick marks every 10% (major) and 2.5% (minor), with 0, half and full capacity below.
    fn scale(&self, ui: &mut Ui) {
        ui.add_space(2.0);
        let width = ui.available_width();
        let (ticks, _) = ui.allocate_exact_size(Vec2::new(width, 5.0), Sense::hover());
        let painter = ui.painter();
        for step in 0..=40 {
            let x = (ticks.left() + ticks.width() * step as f32 / 40.0).round() - 0.5;
            let (height, colour) = if step % 4 == 0 {
                (5.0, theme::DIM)
            } else {
                (3.0, theme::RULE)
            };
            painter.vline(
                x.max(ticks.left() + 0.5),
                ticks.top()..=ticks.top() + height,
                Stroke::new(1.0, colour),
            );
        }
        ui.add_space(2.0);
        let total = self.layout.geometry.usable.len;
        let half = bootshift_core::units::Sectors(total.0 / 2);
        let labels = [
            "0".to_string(),
            size(self.layout, half),
            size(self.layout, total),
        ];
        let galleys: Vec<_> = labels
            .into_iter()
            .map(|text| widgets::galley(ui, text, theme::MONO, 9.6, theme::FAINT))
            .collect();
        let height = galleys.iter().map(|g| g.size().y).fold(0.0, f32::max);
        let (row, _) = ui.allocate_exact_size(Vec2::new(width, height), Sense::hover());
        let painter = ui.painter();
        let anchors = [
            (Align2::LEFT_TOP, row.left_top()),
            (Align2::CENTER_TOP, row.center_top()),
            (Align2::RIGHT_TOP, row.right_top()),
        ];
        for (galley, (align, pos)) in galleys.into_iter().zip(anchors) {
            widgets::text_at(painter, align, pos, galley);
        }
    }

    /// Draws `block` enlarged to a legible width, centred on where it sits and lifted above its
    /// neighbours. Visual only: the partition's real size and the layout are untouched.
    fn magnify(&self, ui: &Ui, inner: Rect, block: &Block) {
        let Some(partition) = self.layout.partition(block.guid) else {
            return;
        };
        if block.rect.width() >= MAGNIFY_MIN {
            return;
        }
        let extent = self.extent_of(partition);
        let centre_x = block.rect.center().x.clamp(
            inner.left() + MAGNIFY_MIN / 2.0,
            inner.right() - MAGNIFY_MIN / 2.0,
        );
        let rect = Rect::from_center_size(
            Pos2::new(centre_x, block.rect.center().y),
            Vec2::new(MAGNIFY_MIN, block.rect.height() + MAGNIFY_LIFT * 2.0),
        );
        let painter = ui.painter_at(rect.expand(8.0));
        painter.rect_filled(
            rect.translate(Vec2::new(0.0, 2.0)),
            4,
            theme::alpha(Color32::from_rgb(4, 4, 6), 0.35),
        );
        painter.rect_filled(rect, 4, brighten(theme::block_colour(partition.content)));
        let used_share = (partition.used.0 as f32 / extent.len.0.max(1) as f32).min(1.0);
        if used_share > 0.0 {
            let used = Rect::from_min_size(
                rect.min,
                Vec2::new(rect.width() * used_share, rect.height()),
            );
            painter.with_clip_rect(rect).rect_filled(
                used,
                0,
                brighten(theme::content_colour(partition.content)),
            );
        }
        if block.locked {
            hatch(&painter, rect);
        }
        painter.rect_stroke(rect, 4, Stroke::new(1.0, theme::SIGNAL), StrokeKind::Inside);
        self.paint_label(ui, &painter, partition, extent, rect);
    }
}

struct Block {
    guid: Guid,
    /// Where the block is drawn.
    rect: Rect,
    /// Where it takes the pointer: its rect minus any sliver drawn over it.
    hit: Rect,
    sliver: bool,
    movable: bool,
    locked: bool,
}

struct BlockResponse {
    body: Response,
    handles: Option<(Response, Response)>,
}

struct Capsule {
    guid: Guid,
    label: String,
    anchor: f32,
    width: f32,
    name: std::sync::Arc<egui::Galley>,
    size: std::sync::Arc<egui::Galley>,
    dot: Color32,
    pickable: bool,
}

fn apply(
    layout: &Layout,
    origin: &Target,
    guid: Guid,
    kind: Drag,
    from: Extent,
    delta: f64,
) -> Vec<(Guid, Extent)> {
    let start = from.start.0 as f64;
    let end = from.end().0 as f64;
    let one = |extent: Option<Extent>| extent.map(|e| (guid, e)).into_iter().collect();
    match kind {
        Drag::Move => one(editing::move_to(layout, origin, guid, start + delta)),
        Drag::Start => one(editing::set_start(layout, origin, guid, start + delta)),
        // The end edge pushes movable neighbours ahead of it, so it can change several partitions.
        Drag::End => editing::push_end(layout, origin, guid, end + delta),
    }
}

pub fn is_sliver(layout: &Layout, extent: Extent) -> bool {
    let usable = layout.geometry.usable.len.0.max(1) as f64;
    (extent.len.0 as f64 / usable) < SLIVER_SHARE
}

/// Where a block sits in a bar `width` points wide: its left offset and its width.
pub fn block_span(layout: &Layout, extent: Extent, width: f32, sliver: bool) -> (f32, f32) {
    let usable = layout.geometry.usable;
    let len = usable.len.0.max(1) as f64;
    let left = (extent.start.0.saturating_sub(usable.start.0) as f64 / len) * f64::from(width);
    let span = (extent.len.0 as f64 / len) * f64::from(width);
    let min = if sliver { MIN_SLIVER } else { MIN_BLOCK };
    (left as f32, (span as f32).max(min))
}

/// An edge handle is at most 0.7rem wide and never more than a fifth of its block, so a narrow
/// partition always keeps a middle to grab.
pub fn handle_width(block_width: f32) -> f32 {
    HANDLE_MAX.min(block_width * 0.2)
}

/// Lays capsules out left to right: each centred on its sliver where it fits, never starting
/// before the previous one's end plus a gap, and never past the bar's right edge. Takes each
/// capsule's (anchor, width) and returns each one's left edge.
pub fn place_capsules(capsules: &[(f32, f32)], bar_width: f32) -> Vec<f32> {
    let mut placed = 0.0f32;
    capsules
        .iter()
        .map(|&(anchor, width)| {
            let left = (anchor - width / 2.0).max(placed).min(bar_width - width);
            placed = left + width + CAPSULE_GAP;
            left
        })
        .collect()
}

/// `block` with the part under `sliver` cut off the side it overlaps.
fn carve(block: Rect, sliver: Rect) -> Rect {
    if !block.intersects(sliver) {
        return block;
    }
    let mut carved = block;
    if sliver.center().x <= block.center().x {
        carved.min.x = carved.min.x.max(sliver.right());
    } else {
        carved.max.x = carved.max.x.min(sliver.left());
    }
    if carved.width() <= 0.0 {
        Rect::NOTHING
    } else {
        carved
    }
}

fn brighten(colour: Color32) -> Color32 {
    let lift = |c: u8| (f32::from(c) * 1.14).min(255.0).round() as u8;
    Color32::from_rgb(lift(colour.r()), lift(colour.g()), lift(colour.b()))
}

fn hairlines(painter: &Painter, rect: Rect, every: f32, colour: Color32) {
    let mut x = rect.left() + 0.5;
    while x < rect.right() {
        painter.vline(x, rect.y_range(), Stroke::new(1.0, colour));
        x += every;
    }
}

/// The 45-degree hatch on locked partitions: 2px stripes, 9px apart across the stripes.
fn hatch(painter: &Painter, rect: Rect) {
    let clipped = painter.with_clip_rect(rect.intersect(painter.clip_rect()));
    let stroke = Stroke::new(2.0, theme::alpha(Color32::BLACK, 0.32));
    let period = 9.0 * std::f32::consts::SQRT_2;
    let height = rect.height();
    let mut x = rect.left() - height;
    while x < rect.right() {
        clipped.line_segment(
            [
                Pos2::new(x, rect.bottom()),
                Pos2::new(x + height, rect.top()),
            ],
            stroke,
        );
        x += period;
    }
}

/// A 2-by-3 field of dots in the middle of the block: a sign that it moves, not a target.
fn paint_grip(painter: &Painter, rect: Rect, bright: bool) {
    let width = 11.25f32.min(rect.width() * 0.4);
    let (cell_w, cell_h) = (5.625, 5.25);
    let columns = ((width / cell_w).floor() as usize).clamp(1, 2);
    let colour = theme::alpha(Color32::WHITE, if bright { 0.95 } else { 0.4 });
    let origin = rect.center() - Vec2::new(cell_w * columns as f32 / 2.0, cell_h * 1.5);
    for row in 0..3 {
        for column in 0..columns {
            let centre =
                origin + Vec2::new(cell_w * (column as f32 + 0.5), cell_h * (row as f32 + 0.5));
            painter.circle_filled(
                centre + Vec2::new(0.0, 0.5),
                1.0,
                theme::alpha(Color32::from_rgb(4, 4, 6), 0.5),
            );
            painter.circle_filled(centre, 1.0, colour);
        }
    }
}

/// An edge handle: a scrim, a 2px line flush with the outer edge, and the two short lines that
/// say "grab this edge". It turns amber while hovered or in use.
fn paint_handle(painter: &Painter, rect: Rect, at_start: bool, active: bool) {
    if rect.width() <= 0.0 {
        return;
    }
    let scrim = if active {
        theme::alpha(theme::SIGNAL, 0.25)
    } else {
        theme::alpha(Color32::from_rgb(4, 4, 6), 0.3)
    };
    painter.rect_filled(rect, 0, scrim);
    let edge_colour = if active {
        theme::SIGNAL
    } else {
        theme::alpha(Color32::WHITE, 0.32)
    };
    let edge_x = if at_start {
        rect.left() + 1.0
    } else {
        rect.right() - 1.0
    };
    painter.vline(edge_x, rect.y_range(), Stroke::new(2.0, edge_colour));
    let grip = theme::alpha(Color32::WHITE, if active { 1.0 } else { 0.8 });
    let middle = rect.center();
    let half = 15.75 / 2.0;
    for x in [middle.x - 1.5, middle.x + 1.5] {
        painter.vline(
            x,
            (middle.y - half)..=(middle.y + half),
            Stroke::new(1.0, grip),
        );
    }
}

fn paint_selection(painter: &Painter, block: &Block) {
    let rect = block.rect;
    painter.rect_stroke(rect, 0, Stroke::new(1.0, theme::SIGNAL), StrokeKind::Inside);
    painter.rect_stroke(
        rect,
        0,
        Stroke::new(1.0, theme::alpha(theme::SIGNAL, 0.3)),
        StrokeKind::Outside,
    );
}

#[cfg(test)]
mod tests {
    use crate::demo::demo_layout;

    use super::*;

    fn target_of(layout: &Layout) -> Target {
        layout
            .partitions
            .iter()
            .map(|p| (p.unique_guid, p.extent))
            .collect()
    }

    #[test]
    fn the_demo_slivers_are_the_efi_reserved_and_recovery_partitions() {
        let layout = demo_layout();
        let slivers: Vec<u32> = layout
            .partitions
            .iter()
            .filter(|p| is_sliver(&layout, p.extent))
            .map(|p| p.number)
            .collect();
        assert_eq!(slivers, vec![1, 2, 4]);
    }

    #[test]
    fn blocks_are_placed_by_sector_with_no_gaps_added() {
        let layout = demo_layout();
        let windows = &layout.partitions[2];
        let recovery = &layout.partitions[3];
        let (left, width) = block_span(&layout, windows.extent, 1000.0, false);
        let (next_left, _) = block_span(&layout, recovery.extent, 1000.0, true);
        assert!((left + width - next_left).abs() < 0.01);
    }

    #[test]
    fn a_sliver_is_never_narrower_than_nine_points() {
        let layout = demo_layout();
        let reserved = layout.partitions[1].extent;
        assert_eq!(block_span(&layout, reserved, 1000.0, true).1, 9.0);
    }

    #[test]
    fn edge_handles_leave_a_middle_to_grab() {
        assert_eq!(handle_width(200.0), 10.5);
        assert_eq!(handle_width(20.0), 4.0);
    }

    #[test]
    fn capsules_on_top_of_each_other_are_spread_apart() {
        let lefts = place_capsules(&[(5.0, 100.0), (6.0, 80.0), (300.0, 50.0)], 1000.0);
        assert_eq!(lefts, vec![0.0, 106.0, 275.0]);
    }

    #[test]
    fn a_capsule_never_runs_past_the_right_edge() {
        assert_eq!(place_capsules(&[(995.0, 100.0)], 1000.0), vec![900.0]);
    }

    #[test]
    fn a_neighbours_hit_area_stops_under_a_sliver() {
        let block = Rect::from_min_max(Pos2::new(0.0, 0.0), Pos2::new(100.0, 10.0));
        let sliver = Rect::from_min_max(Pos2::new(95.0, 0.0), Pos2::new(104.0, 10.0));
        assert_eq!(carve(block, sliver).right(), 95.0);
        let sliver = Rect::from_min_max(Pos2::new(-4.0, 0.0), Pos2::new(5.0, 10.0));
        assert_eq!(carve(block, sliver).left(), 5.0);
    }

    #[test]
    fn a_key_step_moves_one_gib() {
        let layout = demo_layout();
        let target = target_of(&layout);
        let swap = &layout.partitions[5];
        let step = (KEY_STEP_BYTES / 512) as f64;
        let moved = apply(
            &layout,
            &target,
            swap.unique_guid,
            Drag::Move,
            swap.extent,
            step,
        );
        assert_eq!(
            moved.first().map(|(_, e)| e.start.0 - swap.extent.start.0),
            Some(2048 * 1024)
        );
        let linux = &layout.partitions[4];
        let boxed_in = apply(
            &layout,
            &target,
            linux.unique_guid,
            Drag::Move,
            linux.extent,
            step,
        );
        assert!(boxed_in.is_empty(), "Linux has neighbours on both sides");
    }
}
