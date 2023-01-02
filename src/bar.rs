use std::collections::BinaryHeap;
use std::ffi::CString;

use pangocairo::cairo;

use wayrs_client::connection::Connection;

use crate::button_manager::ButtonManager;
use crate::config::Config;
use crate::i3bar_protocol::{self, Block, MinWidth};
use crate::ord_adaptor::DefaultLess;
use crate::pointer_btn::PointerBtn;
use crate::protocol::*;
use crate::shared_state::SharedState;
use crate::state::{ComputedBlock, State};
use crate::tags::{compute_tag_label, TagState, TagsInfo};
use crate::text::{self, ComputedText, RenderOptions};

pub struct Bar {
    pub output: WlOutput,
    pub output_reg_name: u32,
    pub configured: bool,
    pub frame_cb: Option<WlCallback>,
    pub width: u32,
    pub height: u32,
    pub scale: i32,
    pub surface: WlSurface,
    pub layer_surface: ZwlrLayerSurfaceV1,
    pub blocks_btns: ButtonManager<(Option<String>, Option<String>)>,
    pub river_output_status: Option<ZriverOutputStatusV1>,
    pub river_control: Option<ZriverControlV1>,
    pub layout_name: Option<String>,
    pub layout_name_computed: Option<ComputedText>,
    pub tags_btns: ButtonManager,
    pub tags_info: TagsInfo,
    pub tags_computed: Vec<ComputedText>,
}

impl Bar {
    pub fn has_tags_provider(&self) -> bool {
        // TODO: add more tags providers
        self.river_output_status.is_some()
    }

    pub fn set_layout_name(&mut self, layout_name: Option<String>) {
        self.layout_name_computed = None;
        self.layout_name = layout_name;
    }

    pub fn click(
        &mut self,
        conn: &mut Connection<State>,
        ss: &mut SharedState,
        button: PointerBtn,
        seat: WlSeat,
        x: f64,
        _y: f64,
    ) -> anyhow::Result<()> {
        if let Some(tag) = self.tags_btns.click(x) {
            if let Some(river_control) = &self.river_control {
                let cmd = match button {
                    PointerBtn::Left => wayrs_client::cstr!("set-focused-tags"),
                    PointerBtn::Right => wayrs_client::cstr!("toggle-focused-tags"),
                    _ => return Ok(()),
                };
                river_control.add_argument(conn, cmd.into());
                river_control.add_argument(conn, CString::new((1u32 << tag).to_string()).unwrap());
                river_control.run_command(conn, seat);
            }
        } else if let Some((name, instance)) = self.blocks_btns.click(x) {
            if let Some(cmd) = &mut ss.status_cmd {
                cmd.send_click_event(&i3bar_protocol::Event {
                    name: name.as_deref(),
                    instance: instance.as_deref(),
                    button,
                    ..Default::default()
                })?;
            }
        }
        Ok(())
    }

    pub fn frame(&mut self, conn: &mut Connection<State>, ss: &mut SharedState) {
        assert!(self.configured);

        let stride = 4 * self.width as i32;
        let width = self.width as i32;
        let height = self.height as i32;
        let width_f = width as f64;
        let height_f = height as f64;

        let (buffer, canvas) = ss.shm.alloc_buffer(
            conn,
            width * self.scale,
            height * self.scale,
            stride * self.scale,
            wl_shm::Format::Argb8888,
        );

        let cairo_surf = unsafe {
            cairo::ImageSurface::create_for_data_unsafe(
                canvas.as_mut_ptr(),
                cairo::Format::ARgb32,
                width * self.scale,
                height * self.scale,
                stride * self.scale,
            )
            .expect("cairo surface")
        };

        let cairo_ctx = cairo::Context::new(&cairo_surf).expect("cairo context");
        cairo_ctx.scale(self.scale as f64, self.scale as f64);
        self.surface.set_buffer_scale(conn, self.scale);

        // Background
        cairo_ctx.save().unwrap();
        cairo_ctx.set_operator(cairo::Operator::Source);
        ss.config.background.apply(&cairo_ctx);
        cairo_ctx.paint().unwrap();
        cairo_ctx.restore().unwrap();

        // Display tags
        let mut offset_left = 0.0;
        if self.has_tags_provider() {
            if self.tags_computed.is_empty() {
                //  TODO make configurable
                for text in ["1", "2", "3", "4", "5", "6", "7", "8", "9"] {
                    self.tags_computed
                        .push(compute_tag_label(text, &ss.config, &cairo_ctx));
                }
            }
            self.tags_btns.clear();
            for (i, label) in self.tags_computed.iter().enumerate() {
                let state = self.tags_info.get_state(i);
                let (bg, fg) = match state {
                    TagState::Urgent => (ss.config.tag_urgent_bg, ss.config.tag_urgent_fg),
                    TagState::Focused => (ss.config.tag_focused_bg, ss.config.tag_focused_fg),
                    TagState::Active => (ss.config.tag_bg, ss.config.tag_fg),
                    TagState::Inactive => {
                        if ss.config.hide_inactive_tags {
                            continue;
                        }
                        (ss.config.tag_inactive_bg, ss.config.tag_inactive_fg)
                    }
                };
                label.render(
                    &cairo_ctx,
                    RenderOptions {
                        x_offset: offset_left,
                        bar_height: height_f,
                        fg_color: fg,
                        bg_color: Some(bg),
                        r_left: if i == 0 || self.tags_info.get_state(i.saturating_sub(1)) != state
                        {
                            ss.config.tags_r
                        } else {
                            0.0
                        },
                        r_right: if i == 8 || self.tags_info.get_state(i + 1) != state {
                            ss.config.tags_r
                        } else {
                            0.0
                        },
                        overlap: 0.0,
                    },
                );
                self.tags_btns.push(offset_left, label.width, i);
                offset_left += label.width;
            }
        }

        // Display layout name
        if ss.config.show_layout_name {
            if let Some(layout_name) = &self.layout_name {
                let text = self.layout_name_computed.get_or_insert_with(|| {
                    ComputedText::new(
                        layout_name,
                        text::Attributes {
                            font: &ss.config.font,
                            padding_left: 25.0,
                            padding_right: 25.0,
                            min_width: None,
                            align: Default::default(),
                            markup: false,
                        },
                        &cairo_ctx,
                    )
                });
                text.render(
                    &cairo_ctx,
                    RenderOptions {
                        x_offset: offset_left,
                        bar_height: height_f,
                        fg_color: ss.config.tag_inactive_fg,
                        bg_color: None,
                        r_left: 0.0,
                        r_right: 0.0,
                        overlap: 0.0,
                    },
                );
                offset_left += text.width;
            }
        }

        // Display the blocks
        render_blocks(
            &cairo_ctx,
            &ss.config,
            &ss.blocks,
            &mut ss.blocks_cache,
            &mut self.blocks_btns,
            offset_left,
            width_f,
            height_f,
        );

        self.surface.attach(conn, buffer, 0, 0);
        self.surface
            .damage_buffer(conn, 0, 0, width * self.scale, height * self.scale);
        self.surface.commit(conn);
    }

    pub fn request_frame(&mut self, conn: &mut Connection<State>) {
        if self.configured && self.frame_cb.is_none() {
            self.frame_cb = Some(self.surface.frame(conn));
            self.surface.commit(conn);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn render_blocks(
    context: &cairo::Context,
    config: &Config,
    blocks: &[Block],
    blocks_cache: &mut Vec<ComputedBlock>,
    buttons: &mut ButtonManager<(Option<String>, Option<String>)>,
    offset_left: f64,
    full_width: f64,
    full_height: f64,
) {
    let comp_min_width = |block: &Block| {
        let markup = block.markup.as_deref() == Some("pango");
        match &block.min_width {
            Some(MinWidth::Pixels(p)) => Some(*p as f64),
            Some(MinWidth::Text(t)) => Some(text::width_of(t, context, markup, &config.font.0)),
            None => None,
        }
    };
    let comp_full = |block: &Block, min_width: Option<f64>| {
        let markup = block.markup.as_deref() == Some("pango");
        text::ComputedText::new(
            &block.full_text,
            text::Attributes {
                font: &config.font,
                padding_left: 0.0,
                padding_right: 0.0,
                min_width,
                align: block.align.unwrap_or_default(),
                markup,
            },
            context,
        )
    };
    let comp_short = |block: &Block, min_width: Option<f64>| {
        let markup = block.markup.as_deref() == Some("pango");
        block.short_text.as_ref().map(|short_text| {
            text::ComputedText::new(
                short_text,
                text::Attributes {
                    font: &config.font,
                    padding_left: 0.0,
                    padding_right: 0.0,
                    min_width,
                    align: block.align.unwrap_or_default(),
                    markup,
                },
                context,
            )
        })
    };

    // update cashe
    if blocks.len() != blocks_cache.len() {
        blocks_cache.clear();
        for block in blocks {
            let mw = comp_min_width(block);
            blocks_cache.push(ComputedBlock {
                block: block.clone(),
                full: comp_full(block, mw),
                short: comp_short(block, mw),
                min_width: mw,
            });
        }
    } else {
        for (block, computed) in blocks.iter().zip(blocks_cache.iter_mut()) {
            if block.min_width != computed.block.min_width || block.markup != computed.block.markup
            {
                let mw = comp_min_width(block);
                *computed = ComputedBlock {
                    block: block.clone(),
                    full: comp_full(block, mw),
                    short: comp_short(block, mw),
                    min_width: mw,
                };
            } else {
                if block.full_text != computed.block.full_text {
                    computed.block.full_text = block.full_text.clone();
                    computed.full = comp_full(block, computed.min_width);
                }
                if block.short_text != computed.block.short_text {
                    computed.block.full_text = block.full_text.clone();
                    computed.short = comp_short(block, computed.min_width);
                }
                computed.block.color = block.color;
                computed.block.background = block.background;
                computed.block.align = block.align;
                computed.block.name = block.name.clone();
                computed.block.instance = block.instance.clone();
                computed.block.separator = block.separator;
                computed.block.separator_block_width = block.separator_block_width;
            }
        }
    }

    #[derive(Debug)]
    struct LogialBlock<'a> {
        blocks: Vec<(&'a ComputedBlock, bool)>,
        delta: f64,
        separator: bool,
        separator_block_width: u8,
    }

    let mut blocks_computed = Vec::new();
    let mut blocks_width = 0.0;
    let mut s_start = 0;
    while s_start < blocks.len() {
        let mut s_end = s_start + 1;
        let series_name = &blocks[s_start].name;
        while s_end < blocks.len()
            && blocks[s_end - 1].separator_block_width == 0
            && &blocks[s_end].name == series_name
        {
            s_end += 1;
        }

        let mut series = LogialBlock {
            blocks: Vec::with_capacity(s_end - s_start),
            delta: 0.0,
            separator: blocks[s_end - 1].separator,
            separator_block_width: blocks[s_end - 1].separator_block_width,
        };

        for comp in &blocks_cache[s_start..s_end] {
            blocks_width += comp.full.width;
            if let Some(short) = &comp.short {
                series.delta += comp.full.width - short.width;
            }
            series.blocks.push((comp, false));
        }
        if s_end != blocks.len() {
            blocks_width += series.separator_block_width as f64;
        }
        blocks_computed.push(series);
        s_start = s_end;
    }

    // Progressively switch to short mode
    if offset_left + blocks_width > full_width {
        let mut heap = BinaryHeap::new();
        for (i, b) in blocks_computed.iter().enumerate() {
            if b.delta > 0.0 {
                heap.push((DefaultLess(b.delta), i));
            }
        }
        while let Some((DefaultLess(delta), to_switch)) = heap.pop() {
            for comp in &mut blocks_computed[to_switch].blocks {
                comp.1 = true;
            }
            blocks_width -= delta;
            if offset_left + blocks_width <= full_width {
                break;
            }
        }
    }

    // Remove all the empy blocks
    for s in &mut blocks_computed {
        s.blocks.retain(|(text, is_short)| {
            (*is_short
                && text
                    .short
                    .as_ref()
                    .map_or(text.full.width > 0.0, |s| s.width > 0.0))
                || (!is_short && text.full.width > 0.0)
        });
    }

    // Render blocks
    buttons.clear();
    let mut j = 0;
    for series in blocks_computed {
        let s_len = series.blocks.len();
        for (i, (computed, is_short)) in series.blocks.into_iter().enumerate() {
            let block = &computed.block;
            let to_render = if is_short {
                computed.short.as_ref().unwrap_or(&computed.full)
            } else {
                &computed.full
            };
            j += 1;
            to_render.render(
                context,
                RenderOptions {
                    x_offset: full_width - blocks_width,
                    bar_height: full_height,
                    fg_color: block.color.unwrap_or(config.color),
                    bg_color: block.background,
                    r_left: if i == 0 { config.blocks_r } else { 0.0 },
                    r_right: if i + 1 == s_len { config.blocks_r } else { 0.0 },
                    overlap: config.blocks_overlap,
                },
            );
            buttons.push(
                full_width - blocks_width,
                to_render.width,
                (block.name.clone(), block.instance.clone()),
            );
            blocks_width -= to_render.width;
        }
        if j != blocks.len() && series.separator_block_width > 0 {
            let w = series.separator_block_width as f64;
            if series.separator && config.separator_width > 0.0 {
                config.separator.apply(context);
                context.set_line_width(config.separator_width);
                context.move_to(full_width - blocks_width + w * 0.5, full_height * 0.1);
                context.line_to(full_width - blocks_width + w * 0.5, full_height * 0.9);
                context.stroke().unwrap();
            }
            blocks_width -= w;
        }
    }
}
