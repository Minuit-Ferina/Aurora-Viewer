//! Compact title controls for desktop floaters.

#[cfg(any(target_os = "linux", target_os = "macos"))]
mod client;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(windows)]
mod win32;

use winit::window::{Window, WindowAttributes, WindowButtons};

#[cfg(any(windows, target_os = "linux", target_os = "macos"))]
const EDGE: f64 = 5.0;

pub(crate) fn attributes(attrs: WindowAttributes) -> WindowAttributes {
    let attrs = attrs.with_enabled_buttons(WindowButtons::all() - WindowButtons::MINIMIZE);
    #[cfg(windows)]
    {
        use winit::platform::windows::WindowAttributesWindows;
        attrs
            .with_decorations(false)
            .with_platform_attributes(Box::new(WindowAttributesWindows::default().with_undecorated_shadow(true)))
    }
    #[cfg(target_os = "macos")]
    {
        use winit::platform::macos::WindowAttributesMacOS;
        attrs.with_platform_attributes(Box::new(WindowAttributesMacOS::default().with_titlebar_hidden(true)))
    }
    #[cfg(target_os = "linux")]
    {
        attrs.with_decorations(false)
    }
    #[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
    attrs
}

pub(crate) struct Chrome {
    custom: bool,
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    client: client::Caption,
    #[cfg(windows)]
    caption: Option<win32::Caption>,
}

#[derive(Clone, Copy, Default)]
pub(crate) struct Caption {
    pub left: f32,
    pub right: f32,
    pub bottom: f32,
    pub toggle_zoom: bool,
}

pub(crate) fn caption_key(viewport: egui::ViewportId) -> egui::Id {
    viewport.0.with("desktop caption hit area")
}
impl Chrome {
    pub(crate) fn new(window: &dyn Window) -> Self {
        #[cfg(windows)]
        let caption = win32::Caption::install(window);
        #[cfg(windows)]
        let custom = caption.is_some();
        #[cfg(not(windows))]
        let custom = {
            let _ = window;
            cfg!(any(target_os = "linux", target_os = "macos"))
        };
        #[cfg(windows)]
        if !custom {
            log::warn!("native UI: Windows caption hit testing unavailable, using system decorations");
            window.set_decorations(true);
        }
        Self {
            custom,
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            client: client::Caption::default(),
            #[cfg(windows)]
            caption,
        }
    }

    pub(crate) fn update(&self, row: Caption, scale: f32, window: &dyn Window) {
        if !self.custom {
            return;
        }
        #[cfg(windows)]
        if let Some(caption) = &self.caption {
            caption.update(window, scale, row.left, row.right, row.bottom);
        }
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        self.client.update(scale, row.left, row.right, row.bottom);
        if row.toggle_zoom {
            #[cfg(target_os = "macos")]
            self.client.toggle_maximized(window);
            #[cfg(not(target_os = "macos"))]
            window.set_maximized(!window.is_maximized());
        }
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub(crate) fn event(&mut self, window: &dyn Window, event: &winit::event::WindowEvent) -> bool {
        self.client.event(window, event)
    }

    #[cfg(target_os = "linux")]
    pub(crate) fn resize_cursor(&self, window: &dyn Window) -> Option<egui::CursorIcon> {
        self.client.resize_cursor(window)
    }

    pub(crate) fn is_maximized(&self, window: &dyn Window) -> bool {
        #[cfg(target_os = "macos")]
        {
            let _ = window;
            self.client.is_maximized()
        }
        #[cfg(not(target_os = "macos"))]
        window.is_maximized()
    }
}
