use keyframe::{ease, functions::EaseInOut};
use std::time::{Duration, Instant};
use wayland_client::{
    delegate_noop,
    globals::{registry_queue_init, GlobalListContents},
    protocol::{
        wl_buffer, wl_callback, wl_compositor, wl_output, wl_pointer, wl_registry, wl_seat, wl_shm,
        wl_shm_pool, wl_surface,
    },
    Connection, Dispatch, Proxy, QueueHandle,
};
use wayland_protocols::{
    ext::idle_notify::v1::client::{ext_idle_notification_v1, ext_idle_notifier_v1},
    wp::{
        single_pixel_buffer::v1::client::wp_single_pixel_buffer_manager_v1,
        viewporter::client::{wp_viewport, wp_viewporter},
    },
};
use wayland_protocols_wlr::{
    layer_shell::v1::client::{zwlr_layer_shell_v1, zwlr_layer_surface_v1},
    output_power_management::v1::client::{zwlr_output_power_manager_v1, zwlr_output_power_v1},
};

const IDLE_TIME: u32 = 3000;
const FADE_TIME: Duration = Duration::from_millis(2000);

struct FadeBlackSurface {
    surface: wl_surface::WlSurface,
    layer_surface: zwlr_layer_surface_v1::ZwlrLayerSurfaceV1,
    viewport: wp_viewport::WpViewport,
    has_first_configure: bool,
    started: Instant,
}

impl FadeBlackSurface {
    fn new(inner: &StateInner, output: &wl_output::WlOutput) -> Self {
        let surface = inner.compositor.create_surface(&inner.qh, ());
        let layer_surface = inner.layer_shell.get_layer_surface(
            &surface,
            Some(output),
            zwlr_layer_shell_v1::Layer::Overlay,
            "fade-to-black".to_string(),
            &inner.qh,
            (),
        );
        layer_surface.set_anchor(zwlr_layer_surface_v1::Anchor::all());
        layer_surface.set_exclusive_zone(-1);
        let viewport = inner.viewporter.get_viewport(&surface, &inner.qh, ());
        surface.commit();
        Self {
            surface,
            layer_surface,
            viewport,
            has_first_configure: false,
            started: Instant::now(),
        }
    }

    fn is_done(&self) -> bool {
        self.started.elapsed() > FADE_TIME
    }

    fn configure(&mut self, inner: &StateInner, width: u32, height: u32) {
        self.viewport.set_destination(width as i32, height as i32);
        if !self.has_first_configure {
            self.update(inner);
            self.has_first_configure = true;
        }
    }

    fn update(&self, inner: &StateInner) {
        let time = self.started.elapsed().as_secs_f64() / FADE_TIME.as_secs_f64();
        let alpha = ease(EaseInOut, 0., u32::MAX as f64, time) as u32;
        let buffer =
            inner
                .single_pixel_buffer_manager
                .create_u32_rgba_buffer(0, 0, 0, alpha, &inner.qh, ());
        self.surface.attach(Some(&buffer), 0, 0);
        self.surface.frame(&inner.qh, self.surface.clone());
        self.surface.damage(0, 0, i32::MAX, i32::MAX);
        self.surface.commit();
        buffer.destroy();
    }
}

impl Drop for FadeBlackSurface {
    fn drop(&mut self) {
        self.viewport.destroy();
        self.layer_surface.destroy();
        self.surface.destroy();
    }
}

struct Output {
    output: wl_output::WlOutput,
    output_power: zwlr_output_power_v1::ZwlrOutputPowerV1,
    fade_surface: Option<FadeBlackSurface>,
}

struct StateInner {
    output_power_manager: zwlr_output_power_manager_v1::ZwlrOutputPowerManagerV1,
    compositor: wl_compositor::WlCompositor,
    layer_shell: zwlr_layer_shell_v1::ZwlrLayerShellV1,
    viewporter: wp_viewporter::WpViewporter,
    single_pixel_buffer_manager: wp_single_pixel_buffer_manager_v1::WpSinglePixelBufferManagerV1,
    shm: wl_shm::WlShm,
    qh: QueueHandle<State>,
}

struct State {
    inner: StateInner,
    outputs: Vec<Output>,
}

impl State {
    fn update_idle(&mut self, is_idle: bool) {
        let mode = if dbg!(is_idle) {
            zwlr_output_power_v1::Mode::Off
        } else {
            zwlr_output_power_v1::Mode::On
        };
        for output in &mut self.outputs {
            if is_idle {
                output.fade_surface = Some(FadeBlackSurface::new(&self.inner, &output.output));
            } else {
                output.fade_surface = None;
                output.output_power.set_mode(zwlr_output_power_v1::Mode::On);
            }
        }
    }
}

fn main() {
    let connection = Connection::connect_to_env().unwrap();
    let (globals, mut event_queue) = registry_queue_init::<State>(&connection).unwrap();
    let qh = event_queue.handle();

    let output_power_manager = globals
        .bind::<zwlr_output_power_manager_v1::ZwlrOutputPowerManagerV1, _, _>(&qh, 1..=1, ())
        .unwrap();

    let idle_notifier = globals
        .bind::<ext_idle_notifier_v1::ExtIdleNotifierV1, _, _>(&qh, 1..=1, ())
        .unwrap();

    let seat = globals
        .bind::<wl_seat::WlSeat, _, _>(&qh, 1..=1, ())
        .unwrap();
    seat.get_pointer(&qh, ()); // XXX

    let shm = globals.bind::<wl_shm::WlShm, _, _>(&qh, 1..=1, ()).unwrap();

    let compositor = globals
        .bind::<wl_compositor::WlCompositor, _, _>(&qh, 1..=1, ())
        .unwrap();

    let layer_shell = globals
        .bind::<zwlr_layer_shell_v1::ZwlrLayerShellV1, _, _>(&qh, 1..=4, ())
        .unwrap();

    let viewporter = globals
        .bind::<wp_viewporter::WpViewporter, _, _>(&qh, 1..=1, ())
        .unwrap();

    let single_pixel_buffer_manager = globals
        .bind::<wp_single_pixel_buffer_manager_v1::WpSinglePixelBufferManagerV1, _, _>(
            &qh,
            1..=1,
            (),
        )
        .unwrap();

    let outputs = globals.contents().with_list(|list| {
        list.iter()
            .filter(|global| global.interface == wl_output::WlOutput::interface().name)
            .map(|global| {
                let output = globals
                    .registry()
                    .bind(global.name, global.version, &qh, ());
                let output_power = output_power_manager.get_output_power(&output, &qh, ());
                Output {
                    output,
                    output_power,
                    fade_surface: None,
                }
            })
            .collect()
    });

    idle_notifier.get_idle_notification(IDLE_TIME, &seat, &qh, ());

    let mut state = State {
        inner: StateInner {
            compositor,
            output_power_manager,
            layer_shell,
            viewporter,
            single_pixel_buffer_manager,
            shm,
            qh,
        },
        outputs,
    };
    loop {
        event_queue.blocking_dispatch(&mut state).unwrap();
    }
}

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for State {
    fn event(
        _: &mut Self,
        _: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        dbg!(event);
    }
}

impl Dispatch<ext_idle_notification_v1::ExtIdleNotificationV1, ()> for State {
    fn event(
        state: &mut Self,
        _: &ext_idle_notification_v1::ExtIdleNotificationV1,
        event: ext_idle_notification_v1::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        let is_idle = match event {
            ext_idle_notification_v1::Event::Idled => true,
            ext_idle_notification_v1::Event::Resumed => false,
            _ => unreachable!(),
        };
        state.update_idle(is_idle);
    }
}

impl Dispatch<zwlr_layer_surface_v1::ZwlrLayerSurfaceV1, ()> for State {
    fn event(
        state: &mut Self,
        obj: &zwlr_layer_surface_v1::ZwlrLayerSurfaceV1,
        event: zwlr_layer_surface_v1::Event,
        _data: &(),
        _conn: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        match event {
            zwlr_layer_surface_v1::Event::Configure {
                serial,
                width,
                height,
            } => {
                for output in &mut state.outputs {
                    if let Some(fade_surface) = &mut output.fade_surface {
                        if &fade_surface.layer_surface == obj {
                            fade_surface.layer_surface.ack_configure(serial);
                            fade_surface.configure(&state.inner, width, height);
                            break;
                        }
                    }
                }
                dbg!(width, height);
            }
            _ => {}
        }
    }
}

impl Dispatch<wl_pointer::WlPointer, ()> for State {
    fn event(
        _: &mut Self,
        pointer: &wl_pointer::WlPointer,
        event: wl_pointer::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wl_pointer::Event::Enter {
                serial,
                surface,
                surface_x,
                surface_y,
            } => {
                println!("set_cursor");
                pointer.set_cursor(serial, None, 0, 0);
            }
            _ => {}
        }
    }
}

impl Dispatch<wl_callback::WlCallback, wl_surface::WlSurface> for State {
    fn event(
        state: &mut Self,
        _: &wl_callback::WlCallback,
        event: wl_callback::Event,
        surface: &wl_surface::WlSurface,
        _: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        match event {
            wl_callback::Event::Done { callback_data } => {
                for output in &mut state.outputs {
                    if let Some(fade_surface) = &mut output.fade_surface {
                        if &fade_surface.surface == surface {
                            if fade_surface.is_done() {
                                output
                                    .output_power
                                    .set_mode(zwlr_output_power_v1::Mode::Off);
                                output.fade_surface = None;
                            } else {
                                fade_surface.update(&state.inner);
                            }
                            break;
                        }
                    }
                }
            }
            _ => {}
        }
    }
}

delegate_noop!(State: ignore wl_output::WlOutput);
delegate_noop!(State: zwlr_output_power_manager_v1::ZwlrOutputPowerManagerV1);
delegate_noop!(State: ignore zwlr_output_power_v1::ZwlrOutputPowerV1);
delegate_noop!(State: ext_idle_notifier_v1::ExtIdleNotifierV1);
delegate_noop!(State: ignore wl_seat::WlSeat); // XXX
delegate_noop!(State: ignore wl_buffer::WlBuffer);
delegate_noop!(State: ignore wl_surface::WlSurface);
delegate_noop!(State: ignore wl_shm::WlShm);
delegate_noop!(State: wl_shm_pool::WlShmPool);
delegate_noop!(State: zwlr_layer_shell_v1::ZwlrLayerShellV1);
delegate_noop!(State: wp_viewporter::WpViewporter);
delegate_noop!(State: wp_viewport::WpViewport);
delegate_noop!(State: wl_compositor::WlCompositor);
delegate_noop!(State: wp_single_pixel_buffer_manager_v1::WpSinglePixelBufferManagerV1);
