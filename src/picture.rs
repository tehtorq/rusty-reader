//! Images that stay sharp at any size.
//!
//! The GPU shrinks a texture by sampling just a few of its pixels, so a large picture drawn
//! small comes out crunchy and aliased. Each picture therefore keeps a ladder of copies, each
//! half the size of the last, and draws from the one nearest the size on screen.

use eframe::egui::{self, Color32, ColorImage, Rect, TextureHandle, TextureOptions, Vec2, pos2};
use image::DynamicImage;
use image::imageops::FilterType;

/// The smallest copy kept, in pixels along its shorter side.
const SMALLEST: u32 = 32;

/// Decode-side half: the ladder of copies, largest first. Slow; call off the UI thread.
pub fn ladder(image: DynamicImage) -> Vec<ColorImage> {
    let mut levels = vec![color_image(&image)];
    let mut image = image;
    while image.width().min(image.height()) / 2 >= SMALLEST {
        image = image.resize_exact(image.width() / 2, image.height() / 2, FilterType::Triangle);
        levels.push(color_image(&image));
    }
    levels
}

fn color_image(image: &DynamicImage) -> ColorImage {
    let rgba = image.to_rgba8();
    ColorImage::from_rgba_unmultiplied([rgba.width() as usize, rgba.height() as usize], rgba.as_raw())
}

pub struct Picture {
    /// Largest first.
    levels: Vec<TextureHandle>,
}

impl Picture {
    pub fn new(ctx: &egui::Context, name: &str, ladder: Vec<ColorImage>) -> Picture {
        let levels = ladder
            .into_iter()
            .enumerate()
            .map(|(i, image)| ctx.load_texture(format!("{name}#{i}"), image, TextureOptions::LINEAR))
            .collect();
        Picture { levels }
    }

    /// The full-size image's dimensions, in pixels.
    pub fn size(&self) -> Vec2 {
        self.levels.first().map_or(Vec2::ZERO, |t| t.size_vec2())
    }

    /// The copy to draw `uv` of the image into `rect`: the smallest still at least as large as
    /// the screen pixels it covers, so it is only ever shrunk a little.
    fn level(&self, ctx: &egui::Context, rect: Rect, uv: Rect) -> &TextureHandle {
        let wanted = rect.width() * ctx.pixels_per_point() / uv.width().max(1e-3);
        self.levels.iter().rev().find(|t| t.size_vec2().x >= wanted).unwrap_or(&self.levels[0])
    }

    pub fn paint(&self, painter: &egui::Painter, rect: Rect, tint: Color32) {
        let uv = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
        painter.image(self.level(painter.ctx(), rect, uv).id(), rect, uv, tint);
    }

    /// Paint part of the image, rounded at the corners.
    pub fn paint_cropped(
        &self,
        ui: &egui::Ui,
        rect: Rect,
        uv: Rect,
        radius: egui::CornerRadius,
        tint: Color32,
    ) {
        egui::Image::from_texture(self.level(ui.ctx(), rect, uv))
            .uv(uv)
            .corner_radius(radius)
            .tint(tint)
            .paint_at(ui, rect);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_copy_is_half_the_last() {
        let image = DynamicImage::new_rgba8(400, 100);
        let sizes: Vec<_> = ladder(image).iter().map(|i| i.size).collect();
        assert_eq!(sizes, [[400, 100], [200, 50]]);
    }
}
