//! Terminal parsing, cell extraction, and input encoding adapted from herdr-app.
use ghostty_sys::*;
use std::{ffi::c_void, sync::Arc};
const BRACKETED_PASTE: GhosttyMode = 2004;
const SYNC_OUTPUT: GhosttyMode = 2026;
/// How long a program may hold its screen, as in herdr-app and Ghostty.
const MAX_HOLD: std::time::Duration = std::time::Duration::from_millis(1000);
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Rgb(pub u8, pub u8, pub u8);

impl From<GhosttyColorRgb> for Rgb {
    fn from(c: GhosttyColorRgb) -> Self {
        Rgb(c.r, c.g, c.b)
    }
}

#[derive(Clone, Default, PartialEq)]
pub struct Cell {
    /// Empty for blank cells and for the tail half of a wide character.
    pub text: String,
    pub fg: Rgb,
    pub bg: Option<Rgb>,
    pub bold: bool,
    pub italic: bool,
    pub faint: bool,
    pub underline: bool,
    pub strike: bool,
    pub wide: bool,
    /// The foreground is the terminal's default, drawn in the host's own color.
    pub default_fg: bool,
}

#[derive(Clone)]
pub struct Frame {
    pub rows: Vec<Arc<Vec<Cell>>>,
    pub cursor: Option<(u16, u16)>,
    pub bg: Rgb,
}
#[derive(Clone, Debug, PartialEq)]
pub struct Placement {
    pub image: u32,
    /// Changes whenever the image's pixels do (cache key).
    pub generation: u64,
    pub col: i32,
    pub row: i32,
    pub width: u32,
    pub height: u32,
    pub source: [u32; 4],
    pub z: i32,
}

/// PNG decoding for kitty graphics (libghostty-vt leaves it to the embedder).
unsafe extern "C" fn decode_png(
    _ud: *mut c_void,
    alloc: *const GhosttyAllocator,
    data: *const u8,
    len: usize,
    out: *mut GhosttySysImage,
) -> bool {
    let bytes = unsafe { std::slice::from_raw_parts(data, len) };
    let mut decoder = png::Decoder::new(bytes);
    decoder.set_transformations(
        png::Transformations::EXPAND | png::Transformations::STRIP_16 | png::Transformations::ALPHA,
    );
    let Ok(mut reader) = decoder.read_info() else {
        return false;
    };
    if reader.output_buffer_size() > 16 * 1024 * 1024 {
        return false;
    }
    let mut buf = vec![0; reader.output_buffer_size()];
    let Ok(info) = reader.next_frame(&mut buf) else {
        return false;
    };
    let (w, h) = (info.width, info.height);
    let rgba: Vec<u8> = match info.color_type {
        png::ColorType::Rgba => buf[..info.buffer_size()].to_vec(),
        png::ColorType::GrayscaleAlpha => buf[..info.buffer_size()]
            .chunks(2)
            .flat_map(|p| [p[0], p[0], p[0], p[1]])
            .collect(),
        png::ColorType::Rgb => buf[..info.buffer_size()]
            .chunks(3)
            .flat_map(|p| [p[0], p[1], p[2], 255])
            .collect(),
        png::ColorType::Grayscale => buf[..info.buffer_size()]
            .iter()
            .flat_map(|g| [*g, *g, *g, 255])
            .collect(),
        _ => return false,
    };
    let ptr = unsafe { ghostty_alloc(alloc, rgba.len()) };
    if ptr.is_null() {
        return false;
    }
    unsafe {
        std::ptr::copy_nonoverlapping(rgba.as_ptr(), ptr, rgba.len());
        *out = GhosttySysImage {
            width: w,
            height: h,
            data: ptr,
            data_len: rgba.len(),
        };
    }
    true
}

fn install_png_decoder() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| unsafe {
        let f: GhosttySysDecodePngFn = Some(decode_png);
        ghostty_sys_set(
            GHOSTTY_SYS_OPT_DECODE_PNG,
            std::mem::transmute::<GhosttySysDecodePngFn, *const c_void>(f),
        );
    });
}

/// Program copies use the same host clipboard as mouse and copy-mode selections.
unsafe extern "C" fn on_clipboard_write(
    _term: GhosttyTerminal,
    _userdata: *mut c_void,
    request: *const GhosttyClipboardWrite,
) {
    let request = unsafe { &*request };
    let result = (|| {
        if request.location != GHOSTTY_CLIPBOARD_LOCATION_STANDARD || request.contents_len > 1 {
            return Err(GHOSTTY_CLIPBOARD_WRITE_RESULT_UNSUPPORTED);
        }
        let text = if request.contents_len == 0 {
            ""
        } else {
            let content = unsafe { &*request.contents };
            if content.mime.len != b"text/plain".len()
                || unsafe { std::slice::from_raw_parts(content.mime.ptr, content.mime.len) }
                    != b"text/plain"
            {
                return Err(GHOSTTY_CLIPBOARD_WRITE_RESULT_UNSUPPORTED);
            }
            let bytes = if content.data.len == 0 {
                &[]
            } else {
                unsafe { std::slice::from_raw_parts(content.data.ptr, content.data.len) }
            };
            std::str::from_utf8(bytes).map_err(|_| GHOSTTY_CLIPBOARD_WRITE_RESULT_INVALID_DATA)?
        };
        crate::clipboard::copy(text).map_err(|_| GHOSTTY_CLIPBOARD_WRITE_RESULT_IO_ERROR)
    })();
    if let Some(reply) = request.reply {
        let status = GhosttyClipboardWriteReply {
            size: std::mem::size_of::<GhosttyClipboardWriteReply>(),
            result: result
                .err()
                .unwrap_or(GHOSTTY_CLIPBOARD_WRITE_RESULT_SUCCESS),
            remember: false,
        };
        unsafe { reply(request, &status) };
    }
}

/// Synchronized output (mode 2026), copied from herdr-app: when a program
/// starts drawing a frame, the render state keeps its finished frame until it ends.
struct Hold {
    state: GhosttyRenderState,
    since: Option<std::time::Instant>,
}
unsafe extern "C" fn on_render_hold(term: GhosttyTerminal, userdata: *mut c_void, held: bool) {
    let hold = unsafe { &mut *(userdata as *mut Hold) };
    if held {
        // Capture the finished frame the program wants on screen before it redraws.
        unsafe { ghostty_render_state_update(hold.state, term) };
        hold.since = Some(std::time::Instant::now());
    } else {
        hold.since = None;
    }
}

pub struct Vt {
    /// Boxed so its address, given to libghostty as callback userdata, stays fixed.
    hold: Box<Hold>,
    /// A requested local clear waiting for the stream to reach ground.
    clear_pending: bool,
    cached_rows: Vec<Arc<Vec<Cell>>>,
    cached_colors: Option<(Rgb, Rgb, u16, u16)>,
    term: GhosttyTerminal,
    state: GhosttyRenderState,
    rows_it: GhosttyRenderStateRowIterator,
    cells: GhosttyRenderStateRowCells,
    keys: GhosttyKeyEncoder,
    key_event: GhosttyKeyEvent,
    mouse: GhosttyMouseEncoder,
    mouse_event: GhosttyMouseEvent,
    pub cols: u16,
    pub rows: u16,
}
fn ok(result: GhosttyResult, operation: &str) {
    assert_eq!(result, GHOSTTY_SUCCESS, "{operation}");
}
impl Vt {
    pub fn new(cols: u16, rows: u16) -> Self {
        install_png_decoder();
        unsafe {
            let mut vt = Self {
                hold: Box::new(Hold {
                    state: std::ptr::null_mut(),
                    since: None,
                }),
                clear_pending: false,
                cached_rows: Vec::new(),
                cached_colors: None,
                term: std::ptr::null_mut(),
                state: std::ptr::null_mut(),
                rows_it: std::ptr::null_mut(),
                cells: std::ptr::null_mut(),
                keys: std::ptr::null_mut(),
                key_event: std::ptr::null_mut(),
                mouse: std::ptr::null_mut(),
                mouse_event: std::ptr::null_mut(),
                cols: cols.max(1),
                rows: rows.max(1),
            };
            ok(
                ghostty_terminal_new(std::ptr::null(), &mut vt.term, vt.cols, vt.rows),
                "terminal",
            );
            ok(
                ghostty_render_state_new(std::ptr::null(), &mut vt.state),
                "render state",
            );
            vt.hold.state = vt.state;
            let hold: *mut Hold = &mut *vt.hold;
            ghostty_terminal_set(
                vt.term,
                GHOSTTY_TERMINAL_OPT_USERDATA,
                hold as *const c_void,
            );
            let callback: GhosttyTerminalRenderHoldFn = Some(on_render_hold);
            ghostty_terminal_set(
                vt.term,
                GHOSTTY_TERMINAL_OPT_RENDER_HOLD,
                std::mem::transmute::<GhosttyTerminalRenderHoldFn, *const c_void>(callback),
            );
            let callback: GhosttyTerminalClipboardWriteFn = Some(on_clipboard_write);
            ok(
                ghostty_terminal_set(
                    vt.term,
                    GHOSTTY_TERMINAL_OPT_CLIPBOARD_WRITE,
                    std::mem::transmute::<GhosttyTerminalClipboardWriteFn, *const c_void>(callback),
                ),
                "clipboard write callback",
            );
            ok(
                ghostty_render_state_row_iterator_new(std::ptr::null(), &mut vt.rows_it),
                "rows",
            );
            ok(
                ghostty_render_state_row_cells_new(std::ptr::null(), &mut vt.cells),
                "cells",
            );
            ok(
                ghostty_key_encoder_new(std::ptr::null(), &mut vt.keys),
                "keys",
            );
            ok(
                ghostty_key_event_new(std::ptr::null(), &mut vt.key_event),
                "key event",
            );
            ok(
                ghostty_mouse_encoder_new(std::ptr::null(), &mut vt.mouse),
                "mouse",
            );
            ok(
                ghostty_mouse_event_new(std::ptr::null(), &mut vt.mouse_event),
                "mouse event",
            );
            let images: usize = 16 * 1024 * 1024;
            ghostty_terminal_set(
                vt.term,
                GHOSTTY_TERMINAL_OPT_KITTY_IMAGE_STORAGE_LIMIT,
                &images as *const _ as *const c_void,
            );
            let lines: usize = 10_000;
            ghostty_terminal_set(
                vt.term,
                GHOSTTY_TERMINAL_OPT_SCROLLBACK_MAX_LINES,
                &lines as *const _ as *const c_void,
            );
            ghostty_terminal_resize(vt.term, vt.cols, vt.rows, 1, 2);
            let limit: usize = 10_000_000;
            ok(
                ghostty_terminal_set(
                    vt.term,
                    GHOSTTY_TERMINAL_OPT_SCROLLBACK_MAX_BYTES,
                    &limit as *const _ as *const c_void,
                ),
                "scrollback limit",
            );
            vt
        }
    }
    /// Visible kitty graphics placements, bottom layer first.
    pub fn placements(&self) -> Vec<Placement> {
        let mut out = Vec::new();
        unsafe {
            let mut g: GhosttyKittyGraphics = std::ptr::null_mut();
            if ghostty_terminal_get(
                self.term,
                GHOSTTY_TERMINAL_DATA_KITTY_GRAPHICS,
                &mut g as *mut _ as *mut c_void,
            ) != GHOSTTY_SUCCESS
                || g.is_null()
            {
                return out;
            }
            let mut it: GhosttyKittyGraphicsPlacementIterator = std::ptr::null_mut();
            if ghostty_kitty_graphics_placement_iterator_new(std::ptr::null(), &mut it)
                != GHOSTTY_SUCCESS
            {
                return out;
            }
            if ghostty_kitty_graphics_get(
                g,
                GHOSTTY_KITTY_GRAPHICS_DATA_PLACEMENT_ITERATOR,
                &mut it as *mut _ as *mut c_void,
            ) == GHOSTTY_SUCCESS
            {
                while ghostty_kitty_graphics_placement_next(it) {
                    let (mut id, mut z, mut virt) = (0u32, 0i32, false);
                    ghostty_kitty_graphics_placement_get(
                        it,
                        GHOSTTY_KITTY_GRAPHICS_PLACEMENT_DATA_IMAGE_ID,
                        &mut id as *mut _ as *mut c_void,
                    );
                    ghostty_kitty_graphics_placement_get(
                        it,
                        GHOSTTY_KITTY_GRAPHICS_PLACEMENT_DATA_Z,
                        &mut z as *mut _ as *mut c_void,
                    );
                    ghostty_kitty_graphics_placement_get(
                        it,
                        GHOSTTY_KITTY_GRAPHICS_PLACEMENT_DATA_IS_VIRTUAL,
                        &mut virt as *mut _ as *mut c_void,
                    );
                    let img = ghostty_kitty_graphics_image(g, id);
                    if virt || img.is_null() {
                        continue;
                    }
                    let mut info = GhosttyKittyGraphicsPlacementRenderInfo {
                        size: std::mem::size_of::<GhosttyKittyGraphicsPlacementRenderInfo>(),
                        ..Default::default()
                    };
                    if ghostty_kitty_graphics_placement_render_info(it, img, self.term, &mut info)
                        != GHOSTTY_SUCCESS
                        || !info.viewport_visible
                    {
                        continue;
                    }
                    let mut generation = 0u64;
                    ghostty_kitty_graphics_image_get(
                        img,
                        GHOSTTY_KITTY_IMAGE_DATA_GENERATION,
                        &mut generation as *mut _ as *mut c_void,
                    );
                    out.push(Placement {
                        image: id,
                        generation,
                        col: info.viewport_col,
                        row: info.viewport_row,
                        width: info.pixel_width,
                        height: info.pixel_height,
                        source: [
                            info.source_x,
                            info.source_y,
                            info.source_width,
                            info.source_height,
                        ],
                        z,
                    });
                }
            }
            ghostty_kitty_graphics_placement_iterator_free(it);
        }
        out.sort_by_key(|p| p.z);
        out
    }

    /// An image's pixels as BGRA (what GPUI uploads), cropped to `source`.
    pub fn image_bgra(&self, id: u32, source: [u32; 4]) -> Option<(u32, u32, Vec<u8>)> {
        unsafe {
            let mut g: GhosttyKittyGraphics = std::ptr::null_mut();
            ghostty_terminal_get(
                self.term,
                GHOSTTY_TERMINAL_DATA_KITTY_GRAPHICS,
                &mut g as *mut _ as *mut c_void,
            );
            let img = (!g.is_null())
                .then(|| ghostty_kitty_graphics_image(g, id))
                .filter(|i| !i.is_null())?;
            let (mut w, mut h, mut fmt) = (0u32, 0u32, GHOSTTY_KITTY_IMAGE_FORMAT_RGBA);
            let (mut ptr, mut len): (*const u8, usize) = (std::ptr::null(), 0);
            ghostty_kitty_graphics_image_get(
                img,
                GHOSTTY_KITTY_IMAGE_DATA_WIDTH,
                &mut w as *mut _ as *mut c_void,
            );
            ghostty_kitty_graphics_image_get(
                img,
                GHOSTTY_KITTY_IMAGE_DATA_HEIGHT,
                &mut h as *mut _ as *mut c_void,
            );
            ghostty_kitty_graphics_image_get(
                img,
                GHOSTTY_KITTY_IMAGE_DATA_FORMAT,
                &mut fmt as *mut _ as *mut c_void,
            );
            if ghostty_kitty_graphics_image_get(
                img,
                GHOSTTY_KITTY_IMAGE_DATA_DATA_PTR,
                &mut ptr as *mut _ as *mut c_void,
            ) != GHOSTTY_SUCCESS
                || ptr.is_null()
            {
                return None;
            }
            ghostty_kitty_graphics_image_get(
                img,
                GHOSTTY_KITTY_IMAGE_DATA_DATA_LEN,
                &mut len as *mut _ as *mut c_void,
            );
            let px = std::slice::from_raw_parts(ptr, len);
            let bpp = match fmt {
                GHOSTTY_KITTY_IMAGE_FORMAT_RGB => 3,
                GHOSTTY_KITTY_IMAGE_FORMAT_GRAY_ALPHA => 2,
                GHOSTTY_KITTY_IMAGE_FORMAT_GRAY => 1,
                _ => 4,
            };
            let [sx, sy, sw, sh] = source;
            let (sw, sh) = (sw.min(w.saturating_sub(sx)), sh.min(h.saturating_sub(sy)));
            if sw == 0 || sh == 0 {
                return None;
            }
            let mut out = Vec::with_capacity((sw * sh * 4) as usize);
            for y in sy..sy + sh {
                for x in sx..sx + sw {
                    let i = ((y * w + x) as usize) * bpp;
                    let p = px.get(i..i + bpp)?;
                    let (r, g, b, a) = match bpp {
                        4 => (p[0], p[1], p[2], p[3]),
                        3 => (p[0], p[1], p[2], 255),
                        2 => (p[0], p[0], p[0], p[1]),
                        _ => (p[0], p[0], p[0], 255),
                    };
                    out.extend_from_slice(&[b, g, r, a]);
                }
            }
            Some((sw, sh, out))
        }
    }

    pub fn write(&mut self, bytes: &[u8]) {
        let mut bytes = bytes;
        if self.clear_pending {
            let mut consumed = 0;
            let result = unsafe {
                ghostty_terminal_vt_write_until_ground(
                    self.term,
                    bytes.as_ptr(),
                    bytes.len(),
                    &mut consumed,
                )
            };
            if result != GHOSTTY_SUCCESS {
                return;
            }
            bytes = &bytes[consumed..];
            self.clear_at_ground();
        }
        unsafe { ghostty_terminal_vt_write(self.term, bytes.as_ptr(), bytes.len()) };
    }
    fn data<T: Default>(&self, key: GhosttyTerminalData) -> T {
        let mut value = T::default();
        unsafe { ghostty_terminal_get(self.term, key, &mut value as *mut T as *mut c_void) };
        value
    }
    /// Herdr's clear pane for this viewer: history and the rows above the
    /// cursor's line go; that line and the cursor stay, so later output from
    /// the program still lands where it expects. Core's screen is unchanged.
    /// Returns false on the alternate screen, as Herdr does.
    pub fn clear(&mut self) -> bool {
        if self.alt_screen() {
            return false;
        }
        if self.data::<bool>(GHOSTTY_TERMINAL_DATA_VT_GROUND) {
            self.clear_at_ground();
        } else {
            // Out-of-band sequences are safe only between the program's sequences.
            self.clear_pending = true;
        }
        true
    }
    fn clear_at_ground(&mut self) {
        self.clear_pending = false;
        let row = self.data::<u16>(GHOSTTY_TERMINAL_DATA_CURSOR_Y);
        let col = self.data::<u16>(GHOSTTY_TERMINAL_DATA_CURSOR_X);
        let mut sequence = b"\x1b[3J".to_vec();
        if row > 0 {
            // Up a line, to its end, erase everything before; then back down.
            sequence.extend_from_slice(b"\x1b[A\x1b[9999C\x1b[1J\x1b[B");
            sequence.extend_from_slice(format!("\x1b[{}G", col + 1).as_bytes());
        }
        unsafe { ghostty_terminal_vt_write(self.term, sequence.as_ptr(), sequence.len()) };
        self.scroll_to_bottom();
    }
    /// Plain text of history and screen, soft wraps joined, as Herdr hands to `$EDITOR`.
    pub fn text(&self) -> String {
        self.format(std::ptr::null())
    }
    /// The first visible row's position in history and screen rows (Ghostty's screen coordinates).
    pub fn top_row(&self) -> u64 {
        self.scroll_position().0
    }
    /// Plain text between two inclusive `(column, screen row)` points in either order,
    /// as Herdr's `format_selection` reads a selection across history.
    pub fn selection_text(&self, start: (u16, u64), end: (u16, u64)) -> String {
        let grid = |(x, row): (u16, u64)| unsafe {
            let mut point = GhosttyPoint {
                tag: GHOSTTY_POINT_TAG_SCREEN,
                ..Default::default()
            };
            point.value.coordinate = GhosttyPointCoordinate {
                x,
                y: u32::try_from(row).unwrap_or(u32::MAX),
            };
            let mut reference = GhosttyGridRef {
                size: std::mem::size_of::<GhosttyGridRef>(),
                ..Default::default()
            };
            (ghostty_terminal_grid_ref(self.term, point, &mut reference) == GHOSTTY_SUCCESS)
                .then_some(reference)
        };
        let (Some(start), Some(end)) = (grid(start), grid(end)) else {
            return String::new();
        };
        let selection = GhosttySelection {
            size: std::mem::size_of::<GhosttySelection>(),
            start,
            end,
            rectangle: false,
        };
        self.format(&selection)
    }
    fn format(&self, selection: *const GhosttySelection) -> String {
        unsafe {
            let mut formatter: GhosttyFormatter = std::ptr::null_mut();
            let options = GhosttyFormatterTerminalOptions {
                size: std::mem::size_of::<GhosttyFormatterTerminalOptions>(),
                emit: GHOSTTY_FORMATTER_FORMAT_PLAIN,
                unwrap: true,
                trim: true,
                extra: GhosttyFormatterTerminalExtra {
                    size: std::mem::size_of::<GhosttyFormatterTerminalExtra>(),
                    screen: GhosttyFormatterScreenExtra {
                        size: std::mem::size_of::<GhosttyFormatterScreenExtra>(),
                        ..Default::default()
                    },
                    ..Default::default()
                },
                selection,
            };
            if ghostty_formatter_terminal_new(std::ptr::null(), &mut formatter, self.term, options)
                != GHOSTTY_SUCCESS
            {
                return String::new();
            }
            let mut out_ptr = std::ptr::null_mut();
            let mut out_len = 0usize;
            let result = ghostty_formatter_format_alloc(
                formatter,
                std::ptr::null(),
                &mut out_ptr,
                &mut out_len,
            );
            ghostty_formatter_free(formatter);
            let text = if result == GHOSTTY_SUCCESS && out_len > 0 {
                String::from_utf8_lossy(std::slice::from_raw_parts(out_ptr.cast_const(), out_len))
                    .into_owned()
            } else {
                String::new()
            };
            if !out_ptr.is_null() {
                ghostty_free(std::ptr::null(), out_ptr, out_len);
            }
            text
        }
    }
    pub fn resize(&mut self, cols: u16, rows: u16) {
        self.cols = cols.max(1);
        self.rows = rows.max(1);
        unsafe { ghostty_terminal_resize(self.term, self.cols, self.rows, 1, 2) };
    }
    pub fn frame(&mut self) -> Frame {
        unsafe {
            if self
                .hold
                .since
                .is_some_and(|since| since.elapsed() > MAX_HOLD)
            {
                let mode = GhosttyTerminalModeConfig {
                    mode: SYNC_OUTPUT,
                    value: false,
                };
                ghostty_terminal_set(
                    self.term,
                    GHOSTTY_TERMINAL_OPT_MODE,
                    &mode as *const _ as *const c_void,
                );
                self.hold.since = None;
            }
            // While held, draw the frame captured when the hold began.
            if self.hold.since.is_none() {
                ghostty_render_state_update(self.state, self.term);
            }
            let mut colors = GhosttyRenderStateColors {
                size: std::mem::size_of::<GhosttyRenderStateColors>(),
                ..Default::default()
            };
            ghostty_render_state_get(
                self.state,
                GHOSTTY_RENDER_STATE_DATA_COLORS,
                &mut colors as *mut _ as *mut c_void,
            );
            ghostty_render_state_get(
                self.state,
                GHOSTTY_RENDER_STATE_DATA_ROW_ITERATOR,
                &mut self.rows_it as *mut _ as *mut c_void,
            );
            let colors_key = (
                colors.foreground.into(),
                colors.background.into(),
                self.cols,
                self.rows,
            );
            let full = self.cached_colors != Some(colors_key);
            self.cached_rows
                .resize_with(self.rows as usize, || Arc::new(Vec::new()));
            let mut y = 0;
            while if full {
                ghostty_render_state_row_iterator_next(self.rows_it)
            } else {
                ghostty_render_state_row_iterator_next_dirty(self.rows_it, &mut y)
            } {
                ghostty_render_state_row_get(
                    self.rows_it,
                    GHOSTTY_RENDER_STATE_ROW_DATA_CELLS,
                    &mut self.cells as *mut _ as *mut c_void,
                );
                let mut row = Vec::with_capacity(self.cols as usize);
                while ghostty_render_state_row_cells_next(self.cells) {
                    row.push(self.read_cell(colors.foreground.into(), colors.background.into()));
                }
                self.cached_rows[y as usize] = Arc::new(row);
                if full {
                    y += 1;
                }
            }
            self.cached_colors = Some(colors_key);
            let mut cursor = GhosttyRenderStateCursor {
                size: std::mem::size_of::<GhosttyRenderStateCursor>(),
                ..Default::default()
            };
            ghostty_render_state_get(
                self.state,
                GHOSTTY_RENDER_STATE_DATA_CURSOR,
                &mut cursor as *mut _ as *mut c_void,
            );
            ghostty_render_state_clean(self.state);
            let mut frame = Frame {
                rows: self.cached_rows.clone(),
                cursor: (cursor.visible && cursor.viewport_has_value)
                    .then_some((cursor.viewport_x, cursor.viewport_y)),
                bg: colors.background.into(),
            };
            self.paint_images(&mut frame);
            frame
        }
    }
    fn paint_images(&self, frame: &mut Frame) {
        for placement in self.placements() {
            let Some((w, h, pixels)) = self.image_bgra(placement.image, placement.source) else {
                continue;
            };
            let width = placement.width.max(1);
            let height = placement.height.div_ceil(2).max(1);
            for y in 0..height.min(u32::from(self.rows)) {
                for x in 0..width.min(u32::from(self.cols)) {
                    let col = placement.col + x as i32;
                    let row = placement.row + y as i32;
                    if col < 0 || row < 0 {
                        continue;
                    }
                    let Some(cell) = frame
                        .rows
                        .get_mut(row as usize)
                        .and_then(|r| Arc::make_mut(r).get_mut(col as usize))
                    else {
                        continue;
                    };
                    if placement.z < 0 && !cell.text.is_empty() {
                        continue;
                    }
                    let sample = |part: u32| {
                        let sx = x * w / width;
                        let sy = ((y * 2 + part) * h / (height * 2)).min(h - 1);
                        let i = ((sy * w + sx) * 4) as usize;
                        let alpha = u32::from(pixels[i + 3]);
                        let bg = cell.bg.unwrap_or(frame.bg);
                        let mix = |v: u8, b: u8| {
                            ((u32::from(v) * alpha + u32::from(b) * (255 - alpha)) / 255) as u8
                        };
                        Rgb(
                            mix(pixels[i + 2], bg.0),
                            mix(pixels[i + 1], bg.1),
                            mix(pixels[i], bg.2),
                        )
                    };
                    let top = sample(0);
                    let bottom = sample(1);
                    *cell = Cell {
                        text: "▀".into(),
                        fg: top,
                        bg: Some(bottom),
                        ..Cell::default()
                    };
                }
            }
        }
    }

    pub fn scroll_position(&self) -> (u64, u64) {
        unsafe {
            let mut bar = GhosttyTerminalScrollbar::default();
            ghostty_terminal_get(
                self.term,
                GHOSTTY_TERMINAL_DATA_SCROLLBAR,
                &mut bar as *mut _ as *mut c_void,
            );
            (bar.offset, bar.total)
        }
    }
    pub fn scroll_row(&mut self, row: u64) {
        let mut scroll = GhosttyTerminalScrollViewport {
            tag: GHOSTTY_SCROLL_VIEWPORT_ROW,
            ..Default::default()
        };
        scroll.value.row = row as usize;
        unsafe { ghostty_terminal_scroll_viewport(self.term, scroll) };
        // Scrolling shows live content, as Ghostty allows, even while a program holds its frame.
        self.hold.since = None;
    }
    pub fn hyperlink(&self, x: u16, y: u16) -> Option<String> {
        unsafe {
            let mut point = GhosttyPoint {
                tag: GHOSTTY_POINT_TAG_VIEWPORT,
                ..Default::default()
            };
            point.value.coordinate = GhosttyPointCoordinate { x, y: y as u32 };
            let mut reference = GhosttyGridRef {
                size: std::mem::size_of::<GhosttyGridRef>(),
                ..Default::default()
            };
            if ghostty_terminal_grid_ref(self.term, point, &mut reference) != GHOSTTY_SUCCESS {
                return None;
            }
            let mut bytes = vec![0; 4096];
            let mut len = 0;
            if ghostty_grid_ref_hyperlink_uri(&reference, bytes.as_mut_ptr(), bytes.len(), &mut len)
                != GHOSTTY_SUCCESS
                || len == 0
            {
                return None;
            }
            String::from_utf8(bytes[..len].to_vec()).ok()
        }
    }
    unsafe fn read_cell(&mut self, fg_default: Rgb, bg_default: Rgb) -> Cell {
        unsafe {
            let c = self.cells;
            let mut raw: GhosttyCell = Default::default();
            ghostty_render_state_row_cells_get(
                c,
                GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_RAW,
                &mut raw as *mut _ as *mut c_void,
            );
            let mut wide: GhosttyCellWide = GHOSTTY_CELL_WIDE_NARROW;
            ghostty_cell_get(
                raw,
                GHOSTTY_CELL_DATA_WIDE,
                &mut wide as *mut _ as *mut c_void,
            );

            let mut style = GhosttyStyle {
                size: std::mem::size_of::<GhosttyStyle>(),
                ..Default::default()
            };
            ghostty_render_state_row_cells_get(
                c,
                GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_STYLE,
                &mut style as *mut _ as *mut c_void,
            );

            let mut fg = GhosttyColorRgb::default();
            let explicit_fg = ghostty_render_state_row_cells_get(
                c,
                GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_FG_COLOR,
                &mut fg as *mut _ as *mut c_void,
            ) == GHOSTTY_SUCCESS;
            let fg = if explicit_fg {
                Rgb::from(fg)
            } else {
                fg_default
            };
            let mut bg = GhosttyColorRgb::default();
            let bg = (ghostty_render_state_row_cells_get(
                c,
                GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_BG_COLOR,
                &mut bg as *mut _ as *mut c_void,
            ) == GHOSTTY_SUCCESS)
                .then(|| Rgb::from(bg));

            let (mut fg, mut bg) = (fg, bg);
            if style.inverse {
                let new_fg = bg.unwrap_or(bg_default);
                bg = Some(fg);
                fg = new_fg;
            }
            let mut text = String::new();
            if wide != GHOSTTY_CELL_WIDE_SPACER_TAIL
                && wide != GHOSTTY_CELL_WIDE_SPACER_HEAD
                && !style.invisible
            {
                let mut len: u32 = 0;
                ghostty_render_state_row_cells_get(
                    c,
                    GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_GRAPHEMES_LEN,
                    &mut len as *mut _ as *mut c_void,
                );
                if len > 0 {
                    let mut inline = [0u32; 8];
                    let mut overflow = Vec::new();
                    let cps = if len as usize <= inline.len() {
                        &mut inline[..len as usize]
                    } else {
                        overflow.resize(len as usize, 0);
                        &mut overflow
                    };
                    ghostty_render_state_row_cells_get(
                        c,
                        GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_GRAPHEMES_BUF,
                        cps.as_mut_ptr() as *mut c_void,
                    );
                    text = cps.iter().copied().filter_map(char::from_u32).collect();
                }
            }

            Cell {
                text,
                fg,
                bg,
                bold: style.bold,
                italic: style.italic,
                faint: style.faint,
                underline: style.underline != 0,
                strike: style.strikethrough,
                wide: wide == GHOSTTY_CELL_WIDE_WIDE,
                default_fg: !explicit_fg && !style.inverse,
            }
        }
    }

    #[cfg(test)]
    pub fn encode_key(
        &mut self,
        key: GhosttyKey,
        mods: GhosttyMods,
        consumed: GhosttyMods,
        text: Option<&str>,
        unshifted: Option<char>,
    ) -> Vec<u8> {
        self.encode_key_action(
            key,
            mods,
            consumed,
            text,
            unshifted,
            GHOSTTY_KEY_ACTION_PRESS,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn encode_key_action(
        &mut self,
        key: GhosttyKey,
        mods: GhosttyMods,
        consumed: GhosttyMods,
        text: Option<&str>,
        unshifted: Option<char>,
        action: GhosttyKeyAction,
    ) -> Vec<u8> {
        unsafe {
            ghostty_key_encoder_setopt_from_terminal(self.keys, self.term);
            let e = self.key_event;
            ghostty_key_event_set_action(e, action);
            ghostty_key_event_set_key(e, key);
            ghostty_key_event_set_mods(e, mods);
            let printable =
                text.is_some_and(|t| !t.is_empty() && !t.chars().any(|c| c.is_control()));
            ghostty_key_event_set_consumed_mods(e, if printable { consumed } else { 0 });
            match text {
                Some(t) if printable => {
                    ghostty_key_event_set_utf8(e, t.as_ptr() as *const _, t.len())
                }
                _ => ghostty_key_event_set_utf8(e, std::ptr::null(), 0),
            }
            ghostty_key_event_set_unshifted_codepoint(e, unshifted.map_or(0, |c| c as u32));
            let mut buf = [0u8; 128];
            let mut len = 0usize;
            let r = ghostty_key_encoder_encode(
                self.keys,
                e,
                buf.as_mut_ptr() as *mut _,
                buf.len(),
                &mut len,
            );
            if r != GHOSTTY_SUCCESS {
                return Vec::new();
            }
            buf[..len].to_vec()
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn encode_mouse(
        &mut self,
        action: GhosttyMouseAction,
        button: Option<GhosttyMouseButton>,
        mods: GhosttyMods,
        x: f32,
        y: f32,
        // (columns, rows, cell width, cell height), cells in px.
        size: (u32, u32, f32, f32),
        any_pressed: bool,
    ) -> Vec<u8> {
        // Ghostty takes whole-pixel cells; at zoom a cell is 11.7 px, and
        // rounding it alone made the reported column run ahead of the pointer
        // (2 cells by mid-line). So the position moves into a grid of whole
        // cells: same column and row exactly, pixels (SGR-pixels mode) within
        // a few percent.
        let (cols, rows, cw, ch) = size;
        let (cwi, chi) = (cw.round().max(1.0), ch.round().max(1.0));
        let (x, y) = (x / cw.max(1.0) * cwi, y / ch.max(1.0) * chi);
        unsafe {
            ghostty_mouse_encoder_setopt_from_terminal(self.mouse, self.term);
            let s = GhosttyMouseEncoderSize {
                size: std::mem::size_of::<GhosttyMouseEncoderSize>(),
                screen_width: cols * cwi as u32,
                screen_height: rows * chi as u32,
                cell_width: cwi as u32,
                cell_height: chi as u32,
                ..Default::default()
            };
            ghostty_mouse_encoder_setopt(
                self.mouse,
                GHOSTTY_MOUSE_ENCODER_OPT_SIZE,
                &s as *const _ as *const c_void,
            );
            ghostty_mouse_encoder_setopt(
                self.mouse,
                GHOSTTY_MOUSE_ENCODER_OPT_ANY_BUTTON_PRESSED,
                &any_pressed as *const _ as *const c_void,
            );
            let e = self.mouse_event;
            ghostty_mouse_event_set_action(e, action);
            match button {
                Some(b) => ghostty_mouse_event_set_button(e, b),
                None => ghostty_mouse_event_clear_button(e),
            }
            ghostty_mouse_event_set_mods(e, mods);
            ghostty_mouse_event_set_position(e, GhosttyMousePosition { x, y });
            let mut buf = [0u8; 64];
            let mut len = 0usize;
            let r = ghostty_mouse_encoder_encode(
                self.mouse,
                e,
                buf.as_mut_ptr() as *mut _,
                buf.len(),
                &mut len,
            );
            if r != GHOSTTY_SUCCESS {
                return Vec::new();
            }
            buf[..len].to_vec()
        }
    }
    fn get_bool(&self, data: GhosttyTerminalData) -> bool {
        let mut v = false;
        unsafe { ghostty_terminal_get(self.term, data, &mut v as *mut _ as *mut c_void) };
        v
    }

    fn mode(&self, mode: GhosttyMode) -> bool {
        let mut cfg = GhosttyTerminalModeConfig { mode, value: false };
        unsafe {
            ghostty_terminal_get(
                self.term,
                GHOSTTY_TERMINAL_DATA_MODE,
                &mut cfg as *mut _ as *mut c_void,
            )
        };
        cfg.value
    }

    pub fn mouse_tracking(&self) -> bool {
        self.get_bool(GHOSTTY_TERMINAL_DATA_MOUSE_TRACKING)
    }

    pub fn alt_screen(&self) -> bool {
        let mut screen: GhosttyTerminalScreen = GHOSTTY_TERMINAL_SCREEN_PRIMARY;
        unsafe {
            ghostty_terminal_get(
                self.term,
                GHOSTTY_TERMINAL_DATA_ACTIVE_SCREEN,
                &mut screen as *mut _ as *mut c_void,
            )
        };
        screen == GHOSTTY_TERMINAL_SCREEN_ALTERNATE
    }

    pub fn scroll_viewport(&mut self, delta: isize) {
        let mut s = GhosttyTerminalScrollViewport {
            tag: GHOSTTY_SCROLL_VIEWPORT_DELTA,
            ..Default::default()
        };
        s.value.delta = delta;
        unsafe { ghostty_terminal_scroll_viewport(self.term, s) };
    }

    pub fn scroll_to_bottom(&mut self) {
        let s = GhosttyTerminalScrollViewport {
            tag: GHOSTTY_SCROLL_VIEWPORT_BOTTOM,
            ..Default::default()
        };
        unsafe { ghostty_terminal_scroll_viewport(self.term, s) };
    }

    /// Wraps pasted text the way the program asked (bracketed paste or not).
    pub fn paste_bytes(&self, text: &str) -> Vec<u8> {
        if self.mode(BRACKETED_PASTE) {
            let clean = text.replace("\x1b[201~", "");
            [b"\x1b[200~".as_slice(), clean.as_bytes(), b"\x1b[201~"].concat()
        } else {
            text.replace("\r\n", "\r").replace('\n', "\r").into_bytes()
        }
    }
}
impl Drop for Vt {
    fn drop(&mut self) {
        unsafe {
            ghostty_mouse_event_free(self.mouse_event);
            ghostty_mouse_encoder_free(self.mouse);
            ghostty_key_event_free(self.key_event);
            ghostty_key_encoder_free(self.keys);
            ghostty_render_state_row_cells_free(self.cells);
            ghostty_render_state_row_iterator_free(self.rows_it);
            ghostty_render_state_free(self.state);
            ghostty_terminal_free(self.term);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn synchronized_output_shows_the_frame_from_before_the_hold() {
        let mut vt = Vt::new(10, 2);
        let first = |vt: &mut Vt| vt.frame().rows[0][0].text.clone();
        vt.write(b"A");
        assert_eq!(first(&mut vt), "A");
        vt.write(b"\x1b[?2026h\rB");
        assert_eq!(first(&mut vt), "A");
        vt.write(b"\x1b[?2026l");
        assert_eq!(first(&mut vt), "B");
        vt.write(b"\x1b[?2026h\rC");
        vt.hold.since = Some(std::time::Instant::now() - MAX_HOLD * 2);
        assert_eq!(first(&mut vt), "C", "a hold past its limit ends");
    }
    #[test]
    fn graphics_and_links_remain_inside_terminal_cells() {
        let mut vt = Vt::new(20, 4);
        vt.write(b"\x1b]8;;https://example.com\x1b\\link\x1b]8;;\x1b\\");
        assert_eq!(vt.hyperlink(0, 0).as_deref(), Some("https://example.com"));
        vt.write(b"\x1b[2;1H\x1b_Ga=T,f=32,s=1,v=1,c=2,r=1;/wAA/w==\x1b\\");
        let frame = vt.frame();
        assert_eq!(frame.rows[1][0].text, "▀");
        assert_eq!(frame.rows[1][0].fg, Rgb(255, 0, 0));
    }
    #[test]
    fn unicode_colors_cursor_and_alternate_screen() {
        let mut vt = Vt::new(20, 4);
        vt.write(b"X");
        let initial = vt.frame();
        assert_ne!(initial.rows[0][0].fg, initial.bg);
        vt.write(b"\r\x1b[2K");
        vt.write("\x1b[38;2;1;2;3m界e\u{301}".as_bytes());
        let frame = vt.frame();
        assert_eq!(frame.rows[0][0].text, "界");
        assert!(frame.rows[0][0].wide);
        assert_eq!(frame.rows[0][2].text, "e\u{301}");
        assert_eq!(frame.rows[0][0].fg, Rgb(1, 2, 3));
        assert_eq!(frame.cursor, Some((3, 0)));
        vt.write(b"\x1b[?1049h\x1b[HALT");
        assert!(vt.alt_screen());
        assert_eq!(vt.frame().rows[0][0].text, "A");
        vt.write(b"\x1b[?1049l");
        assert!(!vt.alt_screen());
        assert_eq!(vt.frame().rows[0][0].text, "界");
        vt.resize(30, 8);
        assert_eq!(vt.frame().rows.len(), 8);
    }
    #[test]
    fn clean_rows_are_shared_and_old_frames_stay_unchanged() {
        let mut vt = Vt::new(20, 4);
        vt.write(b"first\r\nsecond");
        let before = vt.frame();
        let clean = vt.frame();
        assert!(
            before
                .rows
                .iter()
                .zip(&clean.rows)
                .all(|(a, b)| Arc::ptr_eq(a, b))
        );
        vt.write(b"\x1b[1;1HX");
        let changed = vt.frame();
        assert_eq!(before.rows[0][0].text, "f");
        assert_eq!(changed.rows[0][0].text, "X");
        assert!(Arc::ptr_eq(&before.rows[2], &changed.rows[2]));
        vt.write(b"\x1b[4;1H\r\nlast");
        let scrolled = vt.frame();
        assert_eq!(scrolled.rows[0][0].text, "s");
        assert_eq!(scrolled.rows[3][0].text, "l");
        vt.resize(10, 2);
        let resized = vt.frame();
        assert_eq!(resized.rows.len(), 2);
        assert!(resized.rows.iter().all(|r| r.len() == 10));
    }

    #[test]
    fn retained_frames_match_full_extraction() {
        let mut cached = Vt::new(12, 4);
        let mut full = Vt::new(12, 4);
        for bytes in [
            "one\r\ntwo\r\nthree",
            "\x1b[2;1H界e\u{301}",
            "\x1b[2K",
            "\x1b[4;1Hlast\r\nnext",
            "\x1b[2;4r\x1b[4;1H\n",
            "\x1b[r",
            "\x1b]10;rgb:ff/00/00\x07",
            "\x1b[?1049hALT",
            "\x1b[?1049l",
            "\x1b[2J",
            "\x1b[?25l",
            "\x1b[?25h",
            "\x1bcreset",
        ] {
            cached.write(bytes.as_bytes());
            full.write(bytes.as_bytes());
            full.cached_colors = None;
            let a = cached.frame();
            let b = full.frame();
            assert!(a.rows == b.rows, "{bytes:?}");
            assert_eq!(a.cursor, b.cursor);
            assert_eq!(a.bg, b.bg);
        }
    }

    #[test]
    fn deleted_image_restores_cached_text() {
        let mut vt = Vt::new(20, 4);
        vt.write(b"text\r\x1b_Ga=T,f=32,s=1,v=1,c=2,r=1;/wAA/w==\x1b\\");
        assert_eq!(vt.frame().rows[0][0].text, "▀");
        vt.write(b"\x1b_Ga=d,d=A;\x1b\\");
        assert_eq!(vt.frame().rows[0][0].text, "t");
    }

    #[test]
    fn input_follows_terminal_modes() {
        let mut vt = Vt::new(20, 4);
        assert_eq!(
            vt.encode_key(GHOSTTY_KEY_ARROW_UP, 0, 0, None, None),
            b"\x1b[A"
        );
        vt.write(b"\x1b[?1h\x1b[?2004h\x1b[?1000h\x1b[?1006h");
        assert_eq!(
            vt.encode_key(GHOSTTY_KEY_ARROW_UP, 0, 0, None, None),
            b"\x1bOA"
        );
        assert_eq!(vt.paste_bytes("hello"), b"\x1b[200~hello\x1b[201~");
        assert!(vt.mouse_tracking());
        assert_eq!(
            vt.encode_mouse(
                GHOSTTY_MOUSE_ACTION_PRESS,
                Some(GHOSTTY_MOUSE_BUTTON_LEFT),
                0,
                2.,
                1.,
                (20, 4, 1., 1.),
                false
            ),
            b"\x1b[<0;3;2M"
        );
    }
    #[test]
    fn text_includes_history_and_clear_keeps_the_cursor_line() {
        let mut vt = Vt::new(20, 3);
        for i in 0..10 {
            vt.write(format!("line{i}\r\n").as_bytes());
        }
        vt.write(b"$ typed");
        let text = vt.text();
        assert!(text.contains("line0") && text.contains("$ typed"), "{text}");
        assert!(vt.clear());
        let text = vt.text();
        assert!(!text.contains("line0") && !text.contains("line9"), "{text}");
        assert!(text.contains("$ typed"), "{text}");
        // The cursor stays where the program left it.
        vt.write(b"X");
        assert!(vt.text().contains("$ typedX"));
    }

    #[test]
    fn clear_waits_for_ground_and_skips_the_alternate_screen() {
        let mut vt = Vt::new(20, 3);
        vt.write(b"before\r\n\x1b[");
        assert!(vt.clear());
        vt.write(b"31mred");
        let text = vt.text();
        assert!(
            text.contains("red") && !text.contains("before") && !text.contains("31m"),
            "{text}"
        );
        vt.write(b"\x1b[?1049h");
        assert!(!vt.clear());
    }

    #[test]
    fn selections_span_history_pages() {
        let mut vt = Vt::new(20, 3);
        for i in 0..12 {
            vt.write(format!("line{i:02}\r\n").as_bytes());
        }
        let top = vt.top_row();
        assert!(top >= 9, "{top}");
        // From the second line of history to the third row on screen, across pages.
        let text = vt.selection_text((0, 1), (5, top + 2));
        assert!(
            text.starts_with("line01\n") && text.ends_with("line11"),
            "{text}"
        );
        // Reversed points select the same range.
        assert_eq!(vt.selection_text((5, top + 2), (0, 1)), text);
    }
}
