use std::collections::{HashMap, HashSet};

use anyhow::Context;
use smithay_client_toolkit::{
    reexports::client::{
        Connection, Dispatch, Proxy, QueueHandle, WEnum,
        globals::GlobalList,
        protocol::{wl_output::Transform, wl_shm::Format},
    },
    shm::slot::Buffer,
};
use tracing::{debug, warn};

use super::{
    WaylandClient,
    window_match::{MatchWindow, pair_windows},
};
use crate::wayland_client_event::WaylandClientEvent;
use smithay_client_toolkit::reexports::protocols::ext::{
    foreign_toplevel_list::v1::client::{
        ext_foreign_toplevel_handle_v1::{self, ExtForeignToplevelHandleV1},
        ext_foreign_toplevel_list_v1::{self, ExtForeignToplevelListV1},
    },
    image_capture_source::v1::client::{
        ext_foreign_toplevel_image_capture_source_manager_v1::ExtForeignToplevelImageCaptureSourceManagerV1,
        ext_image_capture_source_v1::{self, ExtImageCaptureSourceV1},
    },
    image_copy_capture::v1::client::{
        ext_image_copy_capture_frame_v1::{self, ExtImageCopyCaptureFrameV1},
        ext_image_copy_capture_manager_v1::{ExtImageCopyCaptureManagerV1, Options},
        ext_image_copy_capture_session_v1::{self, ExtImageCopyCaptureSessionV1},
    },
};

struct WindowIdentity {
    app_id: String,
    title: String,
    ready: bool,
}

impl WindowIdentity {
    fn new() -> Self {
        Self {
            app_id: String::new(),
            title: String::new(),
            ready: false,
        }
    }

    fn set_app_id(&mut self, app_id: &str) {
        if self.app_id != app_id {
            app_id.clone_into(&mut self.app_id);
            self.ready = false;
        }
    }

    fn set_title(&mut self, title: &str) {
        if self.title != title {
            title.clone_into(&mut self.title);
            self.ready = false;
        }
    }

    fn commit(&mut self) {
        self.ready = true;
    }
}

struct ZwlrWindow {
    id: u32,
    identity: WindowIdentity,
}

struct ExtWindow {
    handle: ExtForeignToplevelHandleV1,
    identity: WindowIdentity,
    identifier: String,
}

struct ChosenFormat {
    shm: Format,
    /// Format reported to the existing preview path.
    /// Abgr/Xbgr buffers are swizzled into this layout first.
    reported: Format,
    swap_rb: bool,
}

struct LabwcSession {
    zwlr_id: u32,
    ext_id: u32,
    source: ExtImageCaptureSourceV1,
    width: u32,
    height: u32,
    formats: Vec<Format>,
    frame_requested: bool,
}

struct LabwcFrame {
    zwlr_id: u32,
    ext_id: u32,
    reported: Format,
    swap_rb: bool,
    flip_y: bool,
    buffer: Buffer,
    session: ExtImageCopyCaptureSessionV1,
    source: ExtImageCaptureSourceV1,
}

pub(super) struct LabwcCapture {
    copy_manager: ExtImageCopyCaptureManagerV1,
    source_manager: ExtForeignToplevelImageCaptureSourceManagerV1,
    toplevel_list: ExtForeignToplevelListV1,
    ext_windows: Vec<ExtWindow>,
    zwlr_windows: Vec<ZwlrWindow>,
    sessions: HashMap<ExtImageCopyCaptureSessionV1, LabwcSession>,
    frames: HashMap<ExtImageCopyCaptureFrameV1, LabwcFrame>,
    unsupported: HashSet<u32>,
    // Windows that already produced a preview. The focused window is still
    // refreshed on the timer; the rest are captured once.
    captured: HashSet<u32>,
}

impl std::fmt::Debug for LabwcCapture {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("LabwcCapture")
    }
}

impl LabwcCapture {
    pub(super) fn bind(
        globals: &GlobalList,
        qh: &QueueHandle<WaylandClient>,
    ) -> anyhow::Result<Self> {
        let copy_manager = globals
            .bind::<ExtImageCopyCaptureManagerV1, _, _>(qh, 1..=1, ())
            .context("labwc backend requires ext_image_copy_capture_manager_v1")?;
        let source_manager = globals
            .bind::<ExtForeignToplevelImageCaptureSourceManagerV1, _, _>(qh, 1..=1, ())
            .context(
                "labwc backend requires ext_foreign_toplevel_image_capture_source_manager_v1",
            )?;
        let toplevel_list = globals
            .bind::<ExtForeignToplevelListV1, _, _>(qh, 1..=1, ())
            .context("labwc backend requires ext_foreign_toplevel_list_v1")?;

        Ok(Self {
            copy_manager,
            source_manager,
            toplevel_list,
            ext_windows: Vec::new(),
            zwlr_windows: Vec::new(),
            sessions: HashMap::new(),
            frames: HashMap::new(),
            unsupported: HashSet::new(),
            captured: HashSet::new(),
        })
    }

    fn capture_in_flight(&self, zwlr_id: u32) -> bool {
        self.sessions.values().any(|session| session.zwlr_id == zwlr_id)
            || self.frames.values().any(|frame| frame.zwlr_id == zwlr_id)
    }
}

fn choose_format(formats: &[Format]) -> Option<ChosenFormat> {
    let has = |format: Format| formats.iter().any(|candidate| *candidate == format);

    if has(Format::Argb8888) {
        Some(ChosenFormat {
            shm: Format::Argb8888,
            reported: Format::Argb8888,
            swap_rb: false,
        })
    } else if has(Format::Xrgb8888) {
        Some(ChosenFormat {
            shm: Format::Xrgb8888,
            reported: Format::Xrgb8888,
            swap_rb: false,
        })
    } else if has(Format::Abgr8888) {
        Some(ChosenFormat {
            shm: Format::Abgr8888,
            reported: Format::Argb8888,
            swap_rb: true,
        })
    } else if has(Format::Xbgr8888) {
        Some(ChosenFormat {
            shm: Format::Xbgr8888,
            reported: Format::Xrgb8888,
            swap_rb: true,
        })
    } else if has(Format::Bgr888) {
        Some(ChosenFormat {
            shm: Format::Bgr888,
            reported: Format::Bgr888,
            swap_rb: false,
        })
    } else {
        None
    }
}

fn stride_for(format: Format, width: i32) -> i32 {
    let bytes_per_pixel = match format {
        Format::Bgr888 | Format::Rgb888 => 3,
        _ => 4,
    };
    width * bytes_per_pixel
}

fn swap_red_blue(pixels: &mut [u8]) {
    for pixel in pixels.chunks_exact_mut(4) {
        pixel.swap(0, 2);
    }
}

fn flip_rows(pixels: &mut [u8], stride: usize) {
    if stride == 0 || pixels.len() < stride {
        return;
    }

    let height = pixels.len() / stride;
    let mut row = vec![0u8; stride];
    for y in 0..height / 2 {
        let top = y * stride;
        let bottom = (height - 1 - y) * stride;
        row.copy_from_slice(&pixels[top..top + stride]);
        pixels.copy_within(bottom..bottom + stride, top);
        pixels[bottom..bottom + stride].copy_from_slice(&row);
    }
}

impl WaylandClient {
    pub fn capture_toplevel(&mut self, zwlr_id: u32, qh: &QueueHandle<Self>) -> anyhow::Result<()> {
        let Some(labwc) = &self.labwc else {
            return Ok(());
        };

        if labwc.unsupported.contains(&zwlr_id) || labwc.capture_in_flight(zwlr_id) {
            return Ok(());
        }

        let ext_index = {
            let left: Vec<_> = labwc
                .zwlr_windows
                .iter()
                .map(|window| MatchWindow {
                    app_id: window.identity.app_id.as_str(),
                    title: window.identity.title.as_str(),
                    ready: window.identity.ready,
                    data: window.id,
                })
                .collect();
            let right: Vec<_> = labwc
                .ext_windows
                .iter()
                .enumerate()
                .map(|(index, window)| MatchWindow {
                    app_id: window.identity.app_id.as_str(),
                    title: window.identity.title.as_str(),
                    ready: window.identity.ready,
                    data: index,
                })
                .collect();

            pair_windows(&left, &right)
                .into_iter()
                .find(|(id, _)| *id == zwlr_id)
                .map(|(_, index)| index)
        };

        let Some(ext_index) = ext_index else {
            if let Some(window) = labwc
                .zwlr_windows
                .iter()
                .find(|window| window.id == zwlr_id && window.identity.ready)
            {
                debug!(
                    "labwc capture: no ext toplevel for window {} app_id={:?} title={:?} (zwlr={} ext={})",
                    zwlr_id,
                    window.identity.app_id,
                    window.identity.title,
                    labwc.zwlr_windows.len(),
                    labwc.ext_windows.len()
                );
            }
            return Ok(());
        };

        let (source, session, ext_id) = {
            let labwc = self.labwc.as_ref().unwrap();
            let window = &labwc.ext_windows[ext_index];
            let ext_id = window.handle.id().protocol_id();
            debug!(
                "labwc capture: window {} -> ext {} identifier={} app_id={:?} title={:?}",
                zwlr_id, ext_id, window.identifier, window.identity.app_id, window.identity.title
            );
            let source = labwc
                .source_manager
                .create_source(&window.handle, qh, ());
            let session = labwc
                .copy_manager
                .create_session(&source, Options::empty(), qh, ());
            (source, session, ext_id)
        };

        self.labwc.as_mut().unwrap().sessions.insert(
            session,
            LabwcSession {
                zwlr_id,
                ext_id,
                source,
                width: 0,
                height: 0,
                formats: Vec::new(),
                frame_requested: false,
            },
        );
        Ok(())
    }

    /// Capture every paired toplevel that does not have a preview yet.
    pub fn capture_missing_toplevels(&mut self, qh: &QueueHandle<Self>) -> anyhow::Result<()> {
        let Some(labwc) = &self.labwc else {
            return Ok(());
        };

        let ids: Vec<u32> = labwc
            .zwlr_windows
            .iter()
            .filter(|window| window.identity.ready && !labwc.captured.contains(&window.id))
            .map(|window| window.id)
            .collect();

        for id in ids {
            self.capture_toplevel(id, qh)?;
        }
        Ok(())
    }

    pub(super) fn track_zwlr_window(&mut self, id: u32) {
        let Some(labwc) = &mut self.labwc else {
            return;
        };
        labwc.zwlr_windows.push(ZwlrWindow {
            id,
            identity: WindowIdentity::new(),
        });
    }

    pub(super) fn set_zwlr_title(&mut self, id: u32, title: &str) {
        let Some(window) = self
            .labwc
            .as_mut()
            .and_then(|labwc| labwc.zwlr_windows.iter_mut().find(|window| window.id == id))
        else {
            return;
        };
        window.identity.set_title(title);
    }

    pub(super) fn set_zwlr_app_id(&mut self, id: u32, app_id: &str) {
        let Some(window) = self
            .labwc
            .as_mut()
            .and_then(|labwc| labwc.zwlr_windows.iter_mut().find(|window| window.id == id))
        else {
            return;
        };
        window.identity.set_app_id(app_id);
    }

    pub(super) fn commit_zwlr_window(&mut self, id: u32) {
        let Some(window) = self
            .labwc
            .as_mut()
            .and_then(|labwc| labwc.zwlr_windows.iter_mut().find(|window| window.id == id))
        else {
            return;
        };
        let first = !window.identity.ready;
        window.identity.commit();
        if first {
            debug!(
                "zwlr toplevel {} ready app_id={:?} title={:?}",
                id, window.identity.app_id, window.identity.title
            );
        }
    }

    pub(super) fn remove_zwlr_window(&mut self, id: u32) {
        let Some(labwc) = &mut self.labwc else {
            return;
        };
        labwc.zwlr_windows.retain(|window| window.id != id);
        labwc.unsupported.remove(&id);
        labwc.captured.remove(&id);
    }

    pub(super) fn clear_zwlr_windows(&mut self) {
        let Some(labwc) = &mut self.labwc else {
            return;
        };
        labwc.zwlr_windows.clear();
        labwc.captured.clear();
    }

    fn ext_window_mut(&mut self, handle: &ExtForeignToplevelHandleV1) -> Option<&mut ExtWindow> {
        self.labwc
            .as_mut()?
            .ext_windows
            .iter_mut()
            .find(|window| window.handle == *handle)
    }

    fn close_ext_window(&mut self, handle: &ExtForeignToplevelHandleV1) {
        let Some(labwc) = &mut self.labwc else {
            return;
        };
        let ext_id = handle.id().protocol_id();

        let frame_keys: Vec<_> = labwc
            .frames
            .iter()
            .filter(|(_, frame)| frame.ext_id == ext_id)
            .map(|(key, _)| key.clone())
            .collect();
        for key in frame_keys {
            if let Some(frame) = labwc.frames.remove(&key) {
                key.destroy();
                frame.session.destroy();
                frame.source.destroy();
            }
        }

        let session_keys: Vec<_> = labwc
            .sessions
            .iter()
            .filter(|(_, session)| session.ext_id == ext_id)
            .map(|(key, _)| key.clone())
            .collect();
        for key in session_keys {
            if let Some(session) = labwc.sessions.remove(&key) {
                key.destroy();
                session.source.destroy();
            }
        }

        if let Some(index) = labwc
            .ext_windows
            .iter()
            .position(|window| window.handle == *handle)
        {
            labwc.ext_windows.remove(index).handle.destroy();
        }
    }

    fn destroy_labwc_session(&mut self, session: &ExtImageCopyCaptureSessionV1) {
        let Some(state) = self
            .labwc
            .as_mut()
            .and_then(|labwc| labwc.sessions.remove(session))
        else {
            return;
        };
        session.destroy();
        state.source.destroy();
    }

    fn submit_labwc_frame(
        &mut self,
        session: &ExtImageCopyCaptureSessionV1,
        qh: &QueueHandle<Self>,
    ) {
        let prepared = {
            let Some(session_state) = self
                .labwc
                .as_mut()
                .and_then(|labwc| labwc.sessions.get_mut(session))
            else {
                return;
            };
            if session_state.frame_requested {
                return;
            }
            if session_state.width == 0 || session_state.height == 0 {
                debug!("labwc capture: session has no buffer size yet");
                return;
            }

            let width = session_state.width;
            let height = session_state.height;
            let zwlr_id = session_state.zwlr_id;
            match choose_format(&session_state.formats) {
                None => Err((zwlr_id, session_state.formats.clone())),
                Some(chosen) => match (i32::try_from(width), i32::try_from(height)) {
                    (Ok(width_i), Ok(height_i)) => {
                        session_state.frame_requested = true;
                        Ok((width_i, height_i, chosen.shm, chosen.reported, chosen.swap_rb))
                    }
                    _ => {
                        warn!(
                            "labwc capture: buffer size {width}x{height} does not fit in i32"
                        );
                        Err((zwlr_id, Vec::new()))
                    }
                },
            }
        };

        let (width_i, height_i, shm, reported, swap_rb) = match prepared {
            Ok(prepared) => prepared,
            Err((zwlr_id, formats)) => {
                if formats.is_empty() {
                    warn!("labwc capture: buffer dimensions are unusable for window {zwlr_id}");
                } else if self.labwc.as_mut().unwrap().unsupported.insert(zwlr_id) {
                    warn!(
                        "labwc capture: no supported shm format for window {zwlr_id}: {formats:?}"
                    );
                }
                self.destroy_labwc_session(session);
                return;
            }
        };

        let stride = stride_for(shm, width_i);
        let buffer = match self.pool.create_buffer(width_i, height_i, stride, shm) {
            Ok((buffer, _)) => buffer,
            Err(err) => {
                warn!("labwc capture: failed to allocate buffer: {err}");
                self.destroy_labwc_session(session);
                return;
            }
        };

        let frame = session.create_frame(qh, ());
        frame.attach_buffer(buffer.wl_buffer());
        frame.damage_buffer(0, 0, width_i, height_i);
        frame.capture();

        let Some(session_state) = self
            .labwc
            .as_mut()
            .and_then(|labwc| labwc.sessions.remove(session))
        else {
            frame.destroy();
            return;
        };

        self.labwc.as_mut().unwrap().frames.insert(
            frame,
            LabwcFrame {
                zwlr_id: session_state.zwlr_id,
                ext_id: session_state.ext_id,
                reported,
                swap_rb,
                flip_y: false,
                buffer,
                session: session.clone(),
                source: session_state.source,
            },
        );
    }

    fn finish_labwc_frame(&mut self, frame: &ExtImageCopyCaptureFrameV1, emit: bool) {
        let Some(frame_state) = self
            .labwc
            .as_mut()
            .and_then(|labwc| labwc.frames.remove(frame))
        else {
            return;
        };

        if emit && (frame_state.swap_rb || frame_state.flip_y) {
            if let Some(pixels) = frame_state.buffer.canvas(&mut self.pool) {
                if frame_state.swap_rb {
                    swap_red_blue(pixels);
                }
                if frame_state.flip_y {
                    let stride = frame_state.buffer.stride().max(0) as usize;
                    flip_rows(pixels, stride);
                }
            }
        }

        let LabwcFrame {
            zwlr_id,
            reported,
            buffer,
            session,
            source,
            ..
        } = frame_state;

        frame.destroy();
        session.destroy();
        source.destroy();

        if emit {
            if let Some(labwc) = &mut self.labwc {
                labwc.captured.insert(zwlr_id);
            }
            debug!(
                "labwc capture ready window {zwlr_id} stride={} height={} format={reported:?}",
                buffer.stride(),
                buffer.height()
            );
            self.wl_tx
                .send(WaylandClientEvent::ScreencopyDone(zwlr_id, buffer, reported))
                .unwrap();
        }
    }
}

impl Dispatch<ExtImageCopyCaptureManagerV1, ()> for WaylandClient {
    fn event(
        _: &mut Self,
        _: &ExtImageCopyCaptureManagerV1,
        _: <ExtImageCopyCaptureManagerV1 as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ExtForeignToplevelImageCaptureSourceManagerV1, ()> for WaylandClient {
    fn event(
        _: &mut Self,
        _: &ExtForeignToplevelImageCaptureSourceManagerV1,
        _: <ExtForeignToplevelImageCaptureSourceManagerV1 as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ExtImageCaptureSourceV1, ()> for WaylandClient {
    fn event(
        _: &mut Self,
        _: &ExtImageCaptureSourceV1,
        _: ext_image_capture_source_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ExtForeignToplevelListV1, ()> for WaylandClient {
    fn event(
        state: &mut Self,
        _: &ExtForeignToplevelListV1,
        event: ext_foreign_toplevel_list_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            ext_foreign_toplevel_list_v1::Event::Toplevel { toplevel } => {
                let Some(labwc) = &mut state.labwc else {
                    return;
                };
                labwc.ext_windows.push(ExtWindow {
                    handle: toplevel,
                    identity: WindowIdentity::new(),
                    identifier: String::new(),
                });
            }
            ext_foreign_toplevel_list_v1::Event::Finished => {
                let handles: Vec<_> = state
                    .labwc
                    .as_ref()
                    .map(|labwc| {
                        labwc
                            .ext_windows
                            .iter()
                            .map(|window| window.handle.clone())
                            .collect()
                    })
                    .unwrap_or_default();
                for handle in handles {
                    state.close_ext_window(&handle);
                }
                if let Some(labwc) = &mut state.labwc {
                    labwc.toplevel_list.destroy();
                }
            }
            _ => {}
        }
    }

    smithay_client_toolkit::reexports::client::event_created_child!(
        WaylandClient,
        ExtForeignToplevelListV1,
        [
            ext_foreign_toplevel_list_v1::EVT_TOPLEVEL_OPCODE => (ExtForeignToplevelHandleV1, ())
        ]
    );
}

impl Dispatch<ExtForeignToplevelHandleV1, ()> for WaylandClient {
    fn event(
        state: &mut Self,
        handle: &ExtForeignToplevelHandleV1,
        event: ext_foreign_toplevel_handle_v1::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        match event {
            ext_foreign_toplevel_handle_v1::Event::Title { title } => {
                if let Some(window) = state.ext_window_mut(handle) {
                    window.identity.set_title(&title);
                }
            }
            ext_foreign_toplevel_handle_v1::Event::AppId { app_id } => {
                if let Some(window) = state.ext_window_mut(handle) {
                    window.identity.set_app_id(&app_id);
                }
            }
            ext_foreign_toplevel_handle_v1::Event::Identifier { identifier } => {
                if let Some(window) = state.ext_window_mut(handle) {
                    window.identifier = identifier;
                }
            }
            ext_foreign_toplevel_handle_v1::Event::Done => {
                if let Some(window) = state.ext_window_mut(handle) {
                    let first = !window.identity.ready;
                    window.identity.commit();
                    if first {
                        debug!(
                            "ext toplevel {} ready identifier={} app_id={:?} title={:?}",
                            handle.id().protocol_id(),
                            window.identifier,
                            window.identity.app_id,
                            window.identity.title
                        );
                    }
                }
                let _ = state.capture_missing_toplevels(qh);
            }
            ext_foreign_toplevel_handle_v1::Event::Closed => state.close_ext_window(handle),
            _ => {}
        }
    }
}

impl Dispatch<ExtImageCopyCaptureSessionV1, ()> for WaylandClient {
    fn event(
        state: &mut Self,
        session: &ExtImageCopyCaptureSessionV1,
        event: ext_image_copy_capture_session_v1::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        match event {
            ext_image_copy_capture_session_v1::Event::BufferSize { width, height } => {
                if let Some(session_state) = state
                    .labwc
                    .as_mut()
                    .and_then(|labwc| labwc.sessions.get_mut(session))
                {
                    session_state.width = width;
                    session_state.height = height;
                }
            }
            ext_image_copy_capture_session_v1::Event::ShmFormat { format } => match format {
                WEnum::Value(format) => {
                    if let Some(session_state) = state
                        .labwc
                        .as_mut()
                        .and_then(|labwc| labwc.sessions.get_mut(session))
                    {
                        session_state.formats.push(format);
                    }
                }
                WEnum::Unknown(format) => {
                    debug!("labwc capture: ignoring unknown shm format {format}");
                }
            },
            ext_image_copy_capture_session_v1::Event::DmabufDevice { .. }
            | ext_image_copy_capture_session_v1::Event::DmabufFormat { .. } => {}
            ext_image_copy_capture_session_v1::Event::Done => {
                state.submit_labwc_frame(session, qh);
            }
            ext_image_copy_capture_session_v1::Event::Stopped => {
                debug!("labwc capture session stopped");
                state.destroy_labwc_session(session);
            }
            _ => {}
        }
    }
}

impl Dispatch<ExtImageCopyCaptureFrameV1, ()> for WaylandClient {
    fn event(
        state: &mut Self,
        frame: &ExtImageCopyCaptureFrameV1,
        event: ext_image_copy_capture_frame_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        use ext_image_copy_capture_frame_v1::Event;

        match event {
            Event::Transform { transform } => {
                debug!("labwc capture transform {transform:?}");
                let flip_y = matches!(transform, WEnum::Value(Transform::Flipped));
                if !matches!(
                    transform,
                    WEnum::Value(Transform::Normal) | WEnum::Value(Transform::Flipped)
                ) {
                    debug!("labwc capture: unhandled buffer transform {transform:?}");
                }
                if let Some(frame_state) = state
                    .labwc
                    .as_mut()
                    .and_then(|labwc| labwc.frames.get_mut(frame))
                {
                    frame_state.flip_y = flip_y;
                }
            }
            Event::Damage { .. } | Event::PresentationTime { .. } => {}
            Event::Ready => state.finish_labwc_frame(frame, true),
            Event::Failed { reason } => {
                warn!("labwc window capture failed: {reason:?}");
                state.finish_labwc_frame(frame, false);
            }
            _ => {}
        }
    }
}
