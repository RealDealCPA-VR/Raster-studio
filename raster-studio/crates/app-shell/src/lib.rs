//! The native shell: winit event loop, window, surface, and shortcut routing.
//!
//! Phase 0 wires the vertical slice end-to-end: open a window, create a wgpu
//! surface, load an image into a GPU texture, and drive the [`render::Canvas`]
//! with a pan/zoom [`render::Camera`] via mouse drag + scroll wheel.
//!
//! Input handling here is deliberately minimal; a real shortcut router and the
//! egui integration layer plug into the same [`ApplicationHandler`].

pub mod shortcuts;

use std::sync::Arc;
use std::time::{Duration, Instant};

use glam::Vec2;
use winit::application::ApplicationHandler;
use winit::dpi::PhysicalSize;
use winit::event::{ElementState, KeyEvent, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{Key, ModifiersState};
use winit::window::{Window, WindowId};

use crate::shortcuts::{Action, Chord, Shortcuts};
use editor_core::{Document, History};
use render::{Camera, Canvas, GpuContext, GpuTexture};
use ui::Workspace;

/// An image handed to the shell to display (decoded RGBA8).
pub struct StartupImage {
    pub width: u32,
    pub height: u32,
    pub rgba8: Vec<u8>,
}

/// Per-window GPU state, created once the window exists (winit 0.30 creates
/// windows in `resumed`, so this is built lazily).
struct WindowState {
    window: Arc<Window>,
    surface: wgpu::Surface<'static>,
    surface_config: wgpu::SurfaceConfiguration,
    gpu: GpuContext,
    canvas: Canvas,
    camera: Camera,
    /// The document + history the UI views and edits. Owned by the shell for
    /// Phase 0; a higher app layer takes ownership as the editor grows.
    document: Document,
    history: History,
    workspace: Workspace,
    _source: GpuTexture,
    // egui overlay plumbing.
    egui_ctx: egui::Context,
    egui_state: egui_winit::State,
    egui_renderer: egui_wgpu::Renderer,
    egui_depth: wgpu::TextureView,
}

/// The application: holds the pending image and the live window state.
pub struct App {
    startup_image: Option<StartupImage>,
    state: Option<WindowState>,
    shortcuts: Shortcuts,
    // Input tracking.
    cursor: Vec2,
    dragging: bool,
    /// winit 0.30 delivers modifier state out-of-band via `ModifiersChanged`
    /// rather than on each `KeyEvent`, so the shell mirrors it here.
    modifiers: ModifiersState,
    /// Absolute deadline for the next frame, derived from egui's requested
    /// repaint delay. `None` means a redraw has already been requested and we
    /// are waiting for it to be serviced.
    repaint_at: Option<Instant>,
}

impl App {
    pub fn new(image: StartupImage) -> Self {
        Self {
            startup_image: Some(image),
            state: None,
            shortcuts: Shortcuts::default(),
            cursor: Vec2::ZERO,
            dragging: false,
            modifiers: ModifiersState::empty(),
            repaint_at: Some(Instant::now()),
        }
    }

    /// Translate a winit [`KeyEvent`] into a [`Chord`] and dispatch the resolved
    /// [`Action`] (on key-down, ignoring auto-repeat and non-character keys).
    fn on_keyboard(&mut self, event: KeyEvent) {
        if event.repeat || event.state != ElementState::Pressed {
            return;
        }
        let Key::Character(ch) = event.logical_key else {
            return; // modifier/function keys don't form shortcuts
        };
        let mods = self.modifiers;
        let chord = Chord::new(
            mods.control_key() || mods.super_key(),
            mods.shift_key(),
            mods.alt_key(),
            ch.as_str(),
        );
        if let Some(action) = self.shortcuts.resolve(&chord) {
            self.dispatch_action(action);
        }
    }

    /// Route a resolved keyboard [`Action`]. Phase 0 wires the zoom actions the
    /// shell can already perform; document-bound actions (undo/redo/save/open/
    /// export/new/delete layer) are logged as not-yet-wired until the editor
    /// document bus lands in the app layer above the shell.
    fn dispatch_action(&mut self, action: Action) {
        match action {
            Action::ZoomIn => {
                if let Some(state) = &mut self.state {
                    state.camera.zoom_at(state.camera.viewport_size * 0.5, 1.2);
                }
            }
            Action::ZoomOut => {
                if let Some(state) = &mut self.state {
                    state.camera.zoom_at(state.camera.viewport_size * 0.5, 1.0 / 1.2);
                }
            }
            Action::ZoomFit => {
                if let Some(state) = &mut self.state {
                    state.camera.fit();
                }
            }
            Action::ZoomActualPixels => {
                if let Some(state) = &mut self.state {
                    state.camera.zoom = 1.0;
                }
            }
            other => {
                tracing::debug!("action not wired yet in shell: {other:?}");
            }
        }
    }

    /// Run the event loop. Blocks until the window is closed.
    pub fn run(mut self) -> anyhow::Result<()> {
        let event_loop = EventLoop::new()?;
        event_loop.run_app(&mut self)?;
        Ok(())
    }

    fn redraw(&mut self) {
        let Some(state) = &mut self.state else { return };
        let frame = match state.surface.get_current_texture() {
            Ok(f) => f,
            Err(wgpu::SurfaceError::Lost | wgpu::SurfaceError::Outdated) => {
                state
                    .surface
                    .configure(&state.gpu.device, &state.surface_config);
                // Ask for another frame, or reconfiguring would leave the window
                // frozen once the unconditional redraw loop is gone.
                state.window.request_redraw();
                return;
            }
            Err(e) => {
                tracing::warn!("dropped frame: {e:?}");
                return;
            }
        };
        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        state.canvas.update_camera(&state.gpu, &state.camera);

        // ---- egui frame: run the workspace UI, then apply what it emitted. ----
        let raw_input = state.egui_state.take_egui_input(&state.window);
        let full_output = state.egui_ctx.run(raw_input, |ctx| {
            state.workspace.ui(ctx, &state.document, &state.history);
        });
        state
            .egui_state
            .handle_platform_output(&state.window, full_output.platform_output);
        let mut edited = false;
        for cmd in state.workspace.drain_commands() {
            edited = true;
            if let Err(e) = state.history.apply(&mut state.document, cmd) {
                tracing::warn!("rejected panel command: {e}");
            }
        }
        if edited {
            // The shapes below were tessellated from the pre-command document, so
            // this frame is one edit stale. Schedule an immediate repaint rather
            // than leaving the edit invisible until some later input event.
            state.egui_ctx.request_repaint();
        }
        let paint_jobs =
            state.egui_ctx.tessellate(full_output.shapes, full_output.pixels_per_point);
        let screen_descriptor = egui_wgpu::ScreenDescriptor {
            size_in_pixels: [state.surface_config.width, state.surface_config.height],
            pixels_per_point: state.egui_ctx.pixels_per_point(),
        };

        let mut encoder =
            state
                .gpu
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("frame"),
                });
        state.canvas.render(&mut encoder, &view);

        for (id, delta) in &full_output.textures_delta.set {
            state.egui_renderer.update_texture(
                &state.gpu.device,
                &state.gpu.queue,
                *id,
                delta,
            );
        }
        state.egui_renderer.update_buffers(
            &state.gpu.device,
            &state.gpu.queue,
            &mut encoder,
            &paint_jobs,
            &screen_descriptor,
        );
        {
            let rpass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("egui"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        // MUST be Load. `Operations::default()` is
                        // `LoadOp::Clear(transparent black)`, which wipes the
                        // canvas pass that just drew the image underneath.
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &state.egui_depth,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Discard,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            // egui-wgpu 0.29 needs a `'static` pass and returns `()`.
            let mut rpass = rpass.forget_lifetime();
            state
                .egui_renderer
                .render(&mut rpass, &paint_jobs, &screen_descriptor);
        }
        for id in &full_output.textures_delta.free {
            state.egui_renderer.free_texture(id);
        }

        state.gpu.queue.submit(std::iter::once(encoder.finish()));
        frame.present();

        // Honour egui's own repaint scheduling instead of spinning at vsync
        // forever. ZERO means "another frame now"; anything else is a deadline.
        let delay = full_output
            .viewport_output
            .get(&egui::ViewportId::ROOT)
            .map(|v| v.repaint_delay)
            .unwrap_or(Duration::ZERO);
        // A very long delay means "nothing is animating"; park until an event
        // wakes us rather than scheduling a wakeup years out.
        self.repaint_at = Instant::now().checked_add(delay);
    }

    fn resize(&mut self, size: PhysicalSize<u32>) {
        let Some(state) = &mut self.state else { return };
        if size.width == 0 || size.height == 0 {
            return;
        }
        state.surface_config.width = size.width;
        state.surface_config.height = size.height;
        state
            .surface
            .configure(&state.gpu.device, &state.surface_config);
        state.egui_depth = create_depth_view(&state.gpu, size.width, size.height);
        state.camera.viewport_size = Vec2::new(size.width as f32, size.height as f32);
    }
}

/// Create a depth texture view matching the current surface size. Needed by the
/// egui render pass; recreated on resize.
fn create_depth_view(gpu: &GpuContext, width: u32, height: u32) -> wgpu::TextureView {
    let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("egui-depth"),
        size: wgpu::Extent3d {
            width: width.max(1),
            height: height.max(1),
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Depth32Float,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    });
    texture.create_view(&wgpu::TextureViewDescriptor::default())
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.state.is_some() {
            return;
        }
        let image = self.startup_image.take().expect("startup image present");

        let attrs = Window::default_attributes()
            .with_title("Raster Studio")
            .with_inner_size(PhysicalSize::new(1280, 720));
        let window = Arc::new(event_loop.create_window(attrs).expect("create window"));

        let instance = wgpu::Instance::default();
        let surface = instance
            .create_surface(window.clone())
            .expect("create surface");

        let gpu =
            pollster::block_on(GpuContext::for_surface(instance, &surface)).expect("gpu context");

        let size = window.inner_size();
        let caps = surface.get_capabilities(&gpu.adapter);
        let format = caps
            .formats
            .iter()
            .copied()
            .find(|f| f.is_srgb())
            .unwrap_or(caps.formats[0]);
        let surface_config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode: wgpu::PresentMode::AutoVsync,
            alpha_mode: caps.alpha_modes[0],
            view_formats: vec![],
            desired_maximum_frame_latency: 2,
        };
        surface.configure(&gpu.device, &surface_config);

        let source = GpuTexture::from_rgba8(
            &gpu,
            image.width,
            image.height,
            &image.rgba8,
            "startup-image",
        );
        let mut canvas = Canvas::new(&gpu, format);
        canvas.set_source(&gpu, &source);

        let mut camera = Camera::new(
            Vec2::new(image.width as f32, image.height as f32),
            Vec2::new(size.width as f32, size.height as f32),
        );
        camera.fit();

        // ---- egui overlay setup (egui-winit + egui-wgpu). ----
        let egui_ctx = egui::Context::default();
        let egui_state = egui_winit::State::new(
            egui_ctx.clone(),
            egui::ViewportId::ROOT,
            &*window,
            Some(window.scale_factor() as f32),
            Some(winit::window::Theme::Dark),
            Some(gpu.adapter.limits().max_texture_dimension_2d as usize),
        );
        let egui_renderer = egui_wgpu::Renderer::new(
            &gpu.device,
            format,
            Some(wgpu::TextureFormat::Depth32Float),
            1,
            true,
        );
        let egui_depth = create_depth_view(&gpu, size.width.max(1), size.height.max(1));

        // A minimal document the panels can view/edit; owned here in Phase 0 so
        // the workspace UI has something to render.
        let document = Document::new(image.width, image.height, "Raster Studio");
        let history = History::with_limit(0);
        let workspace = Workspace::default();

        self.state = Some(WindowState {
            window,
            surface,
            surface_config,
            gpu,
            canvas,
            camera,
            document,
            history,
            workspace,
            _source: source,
            egui_ctx,
            egui_state,
            egui_renderer,
            egui_depth,
        });
    }

    /// Idle policy. Without this the shell would redraw at full vsync forever,
    /// burning a core and the battery on a completely static document.
    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        let Some(state) = &self.state else { return };
        match self.repaint_at {
            // No deadline: egui asked for an effectively infinite delay, so the
            // document is static. Sleep until an input event arrives.
            None => event_loop.set_control_flow(ControlFlow::Wait),
            Some(at) if at <= Instant::now() => {
                // Due now. Clear the deadline so we ask exactly once; `redraw`
                // sets the next one.
                self.repaint_at = None;
                state.window.request_redraw();
                event_loop.set_control_flow(ControlFlow::Wait);
            }
            Some(at) => event_loop.set_control_flow(ControlFlow::WaitUntil(at)),
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        // Let egui observe every event (hover, focus, input on panels). When egui
        // consumes an event the pointer is over a panel, so the canvas must not
        // also pan/zoom on it.
        let (consumed, egui_wants_repaint) = match &mut self.state {
            Some(state) => {
                let r = state.egui_state.on_window_event(&state.window, &event);
                (r.consumed, r.repaint)
            }
            None => (false, false),
        };
        // With the unconditional redraw loop gone, input has to schedule its own
        // frame or the window would stay frozen until the next timer fires.
        if egui_wants_repaint {
            self.repaint_at = Some(Instant::now());
        }
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => {
                self.resize(size);
                self.repaint_at = Some(Instant::now());
            }
            WindowEvent::ModifiersChanged(mods) => self.modifiers = mods.state(),
            WindowEvent::RedrawRequested => self.redraw(),
            WindowEvent::KeyboardInput { event: key_event, .. } if !consumed => {
                self.on_keyboard(key_event);
                self.repaint_at = Some(Instant::now());
            }
            WindowEvent::MouseInput { state, button, .. } => {
                if button == MouseButton::Left {
                    // Always honour release, so a drag that ends over a panel
                    // cannot leave the canvas stuck in dragging state.
                    self.dragging = state == ElementState::Pressed && !consumed;
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                let new = Vec2::new(position.x as f32, position.y as f32);
                if self.dragging && !consumed {
                    let delta = new - self.cursor;
                    if let Some(state) = &mut self.state {
                        state.camera.pan_screen(delta);
                    }
                    self.repaint_at = Some(Instant::now());
                }
                self.cursor = new;
            }
            WindowEvent::MouseWheel { delta, .. } if !consumed => {
                let scroll = match delta {
                    MouseScrollDelta::LineDelta(_, y) => y,
                    MouseScrollDelta::PixelDelta(p) => p.y as f32 / 60.0,
                };
                let factor = (1.0 + scroll * 0.1).clamp(0.2, 5.0);
                let anchor = self.cursor;
                if let Some(state) = &mut self.state {
                    state.camera.zoom_at(anchor, factor);
                }
                self.repaint_at = Some(Instant::now());
            }
            _ => {}
        }
    }
}

/// Convenience entry point used by `studio-desktop`.
pub fn launch(image: StartupImage) -> anyhow::Result<()> {
    App::new(image).run()
}
