//! The bundled icons (`assets/icons`), drawn in the widget's text color.
//!
//! GTK recolors `-symbolic.svg` icons by restyling the SVG through librsvg,
//! which leaves them in their file color with some GTK and librsvg
//! versions (GTK 4.14 with librsvg 2.58, for one): dark gray on the dark
//! theme. This paintable rasterizes each icon once as an alpha mask and
//! fills it with the foreground color GTK passes to symbolic paintables, so
//! icons follow the theme, hover, and disabled states everywhere.

use gtk4::gdk_pixbuf::Pixbuf;
use gtk4::prelude::*;
use gtk4::subclass::prelude::*;
use gtk4::{gdk, glib, graphene, Image};
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;

/// Rasterized at this many pixels per icon pixel, for sharp HiDPI output.
const OVERSAMPLE: i32 = 3;

mod imp {
    use super::*;
    use std::cell::{Cell, OnceCell};

    #[derive(Default)]
    pub struct SymbolicIcon {
        pub mask: OnceCell<Option<gdk::Texture>>,
        pub size: Cell<i32>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for SymbolicIcon {
        const NAME: &'static str = "HgsrSymbolicIcon";
        type Type = super::SymbolicIcon;
        type Interfaces = (gdk::Paintable, gtk4::SymbolicPaintable);
    }

    impl ObjectImpl for SymbolicIcon {}

    impl PaintableImpl for SymbolicIcon {
        fn intrinsic_width(&self) -> i32 {
            self.size.get()
        }

        fn intrinsic_height(&self) -> i32 {
            self.size.get()
        }

        fn flags(&self) -> gdk::PaintableFlags {
            gdk::PaintableFlags::STATIC_SIZE | gdk::PaintableFlags::STATIC_CONTENTS
        }

        fn snapshot(&self, snapshot: &gdk::Snapshot, width: f64, height: f64) {
            // Outside a symbolic context: a light gray that reads on both themes.
            self.draw(snapshot, width, height, &gdk::RGBA::new(0.6, 0.62, 0.66, 1.0));
        }
    }

    impl SymbolicPaintableImpl for SymbolicIcon {
        fn snapshot_symbolic(&self, snapshot: &gdk::Snapshot, width: f64, height: f64, colors: &[gdk::RGBA]) {
            let fg = colors.first().copied().unwrap_or(gdk::RGBA::new(0.9, 0.9, 0.92, 1.0));
            self.draw(snapshot, width, height, &fg);
        }
    }

    impl SymbolicIcon {
        fn draw(&self, snapshot: &gdk::Snapshot, width: f64, height: f64, color: &gdk::RGBA) {
            let (Some(Some(mask)), Some(snapshot)) = (self.mask.get(), snapshot.downcast_ref::<gtk4::Snapshot>())
            else {
                return;
            };
            // Every pixel takes the color; the mask supplies only coverage.
            let matrix = graphene::Matrix::from_float([
                0.0, 0.0, 0.0, 0.0, //
                0.0, 0.0, 0.0, 0.0, //
                0.0, 0.0, 0.0, 0.0, //
                0.0, 0.0, 0.0, color.alpha(),
            ]);
            let offset = graphene::Vec4::new(color.red(), color.green(), color.blue(), 0.0);
            snapshot.push_color_matrix(&matrix, &offset);
            snapshot.append_texture(mask, &graphene::Rect::new(0.0, 0.0, width as f32, height as f32));
            snapshot.pop();
        }
    }
}

glib::wrapper! {
    pub struct SymbolicIcon(ObjectSubclass<imp::SymbolicIcon>)
        @implements gdk::Paintable, gtk4::SymbolicPaintable;
}

thread_local! {
    static MASKS: RefCell<HashMap<(String, i32), Option<gdk::Texture>>> = RefCell::default();
}

fn icon_dir() -> Option<&'static PathBuf> {
    static DIR: std::sync::OnceLock<Option<PathBuf>> = std::sync::OnceLock::new();
    DIR.get_or_init(crate::bundled::symbolic_icon_dir).as_ref()
}

#[allow(deprecated)] // `Texture::for_pixbuf`: GTK 4.10 has no other way to wrap a Pixbuf.
fn mask(name: &str, size: i32) -> Option<gdk::Texture> {
    MASKS.with(|masks| {
        masks
            .borrow_mut()
            .entry((name.to_string(), size))
            .or_insert_with(|| {
                let path = icon_dir()?.join(format!("hgsr-{name}-symbolic.svg"));
                let px = size * OVERSAMPLE;
                Pixbuf::from_file_at_size(&path, px, px)
                    .map_err(|err| eprintln!("[HyperGSR] icon {name}: {err}"))
                    .ok()
                    .map(|pixbuf| gdk::Texture::for_pixbuf(&pixbuf))
            })
            .clone()
    })
}

impl SymbolicIcon {
    pub fn new(name: &str, size: i32) -> Self {
        let icon: Self = glib::Object::new();
        icon.imp().size.set(size);
        let _ = icon.imp().mask.set(mask(name, size));
        icon
    }
}

/// An image of the bundled icon `name`, `size` pixels square.
pub fn image(name: &str, size: i32) -> Image {
    let image = Image::from_paintable(Some(&SymbolicIcon::new(name, size)));
    image.set_pixel_size(size);
    image
}

/// Shows the bundled icon `name` in `image`, keeping its size.
pub fn set(image: &Image, name: &str) {
    let size = if image.pixel_size() > 0 { image.pixel_size() } else { 16 };
    image.set_paintable(Some(&SymbolicIcon::new(name, size)));
}
