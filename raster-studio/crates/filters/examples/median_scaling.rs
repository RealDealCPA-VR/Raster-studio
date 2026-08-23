//! One-off check that the histogram median's cost is flat in the radius.
use std::time::Instant;

fn main() {
    let (w, h) = (512u32, 512u32);
    let mut px = Vec::new();
    for y in 0..h {
        for x in 0..w {
            let v = ((x * 7 + y * 13) % 256) as f32 / 255.0;
            px.push([v, 1.0 - v, (v * 3.0).fract(), 1.0]);
        }
    }
    let src = filters::FilterBuffer::from_pixels(w, h, px).unwrap();
    for r in [4u32, 8, 16, 32, 64] {
        let t = Instant::now();
        let out = filters::median(&src, r, filters::EdgeMode::Clamp);
        println!("radius {r:3} -> {:?} (len {})", t.elapsed(), out.len());
    }
}
