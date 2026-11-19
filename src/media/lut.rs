use super::preview::FrameImage;
use std::path::Path;
use std::sync::Arc;

#[derive(Clone)]
pub struct Lut {
    size: usize,
    one_d: bool,
    min: [f64; 3],
    max: [f64; 3],
    rows: Vec<[f64; 3]>,
}

impl Lut {
    pub fn load(path: &Path) -> Result<Self, String> {
        std::fs::read_to_string(path)
            .map_err(|error| error.to_string())
            .and_then(|text| Self::parse(&text))
            .map_err(|error| format!("LUT {}: {error}", path.display()))
    }

    pub fn parse(text: &str) -> Result<Self, String> {
        let mut lut = Self {
            size: 0,
            one_d: false,
            min: [0.0; 3],
            max: [1.0; 3],
            rows: Vec::new(),
        };
        let mut domain_seen = [false; 2];
        for (line_number, line) in text.lines().enumerate() {
            let mut quoted = false;
            let comment = line.char_indices().find_map(|(index, ch)| {
                if ch == '"' {
                    quoted = !quoted;
                }
                (ch == '#' && !quoted).then_some(index)
            });
            let line = line[..comment.unwrap_or(line.len())].trim();
            if line.is_empty() {
                continue;
            }
            let fields: Vec<_> = line.split_whitespace().collect();
            let error = |message: &str| format!("line {}: {message}", line_number + 1);
            match fields[0] {
                "TITLE" => {
                    let title = line.trim_start_matches("TITLE").trim();
                    if !title.starts_with('"') || !title.ends_with('"') || title.len() < 2 {
                        return Err(error("TITLE must be quoted"));
                    }
                }
                "LUT_3D_SIZE" | "LUT_1D_SIZE" => {
                    if lut.size != 0 {
                        return Err(error("duplicate or mixed LUT sizes"));
                    }
                    if fields.len() != 2 {
                        return Err(error("expected one LUT size"));
                    }
                    lut.one_d = fields[0] == "LUT_1D_SIZE";
                    lut.size = fields[1].parse().map_err(|_| error("invalid LUT size"))?;
                    let limit = if lut.one_d { 65536 } else { 256 };
                    if !(2..=limit).contains(&lut.size) {
                        return Err(error("LUT size outside supported range"));
                    }
                }
                "DOMAIN_MIN" | "DOMAIN_MAX" => {
                    let index = usize::from(fields[0] == "DOMAIN_MAX");
                    if domain_seen[index] || !lut.rows.is_empty() {
                        return Err(error("duplicate or misplaced domain"));
                    }
                    domain_seen[index] = true;
                    let values = triple(&fields[1..]).map_err(|message| error(&message))?;
                    if index == 0 {
                        lut.min = values
                    } else {
                        lut.max = values
                    }
                }
                _ => {
                    if lut.size == 0 {
                        return Err(error("missing LUT size before data"));
                    }
                    let row = triple(&fields).map_err(|message| error(&message))?;
                    let expected = if lut.one_d { lut.size } else { lut.size.pow(3) };
                    if lut.rows.len() == expected {
                        return Err(error("too many LUT data rows"));
                    }
                    lut.rows.push(row);
                }
            }
        }
        if lut.size == 0 {
            return Err("missing LUT size".into());
        }
        if (0..3).any(|c| lut.min[c] >= lut.max[c] || !(lut.max[c] - lut.min[c]).is_finite()) {
            return Err("invalid domain: each maximum must exceed its minimum".into());
        }
        let expected = if lut.one_d { lut.size } else { lut.size.pow(3) };
        if lut.rows.len() != expected {
            return Err(format!(
                "expected {expected} LUT data rows, found {}",
                lut.rows.len()
            ));
        }
        Ok(lut)
    }

    fn rgb(&self, rgb: [u8; 3]) -> [u8; 3] {
        let coords: [f64; 3] = std::array::from_fn(|c| {
            ((f64::from(rgb[c]) / 255.0 - self.min[c]) / (self.max[c] - self.min[c]))
                .clamp(0.0, 1.0)
                * (self.size - 1) as f64
        });
        let lo = coords.map(|v| (v as usize).min(self.size - 2));
        let t: [f64; 3] = std::array::from_fn(|c| coords[c] - lo[c] as f64);
        let lerp = |a: f64, b: f64, t: f64| a * (1.0 - t) + b * t;
        std::array::from_fn(|c| {
            let value = if self.one_d {
                lerp(self.rows[lo[c]][c], self.rows[lo[c] + 1][c], t[c])
            } else {
                let at = |r: usize, g: usize, b: usize| {
                    self.rows[r + self.size * (g + self.size * b)][c]
                };
                let plane = |b| {
                    lerp(
                        lerp(at(lo[0], lo[1], b), at(lo[0] + 1, lo[1], b), t[0]),
                        lerp(at(lo[0], lo[1] + 1, b), at(lo[0] + 1, lo[1] + 1, b), t[0]),
                        t[1],
                    )
                };
                lerp(plane(lo[2]), plane(lo[2] + 1), t[2])
            };
            (value.clamp(0.0, 1.0) * 255.0).round() as u8
        })
    }

    pub fn compile(&self) -> CompiledLut {
        let axes = std::array::from_fn(|c| {
            std::array::from_fn(|v| {
                let coordinate = ((v as f64 / 255.0 - self.min[c]) / (self.max[c] - self.min[c]))
                    .clamp(0.0, 1.0)
                    * (self.size - 1) as f64;
                let index = (coordinate as usize).min(self.size - 2);
                (index, (coordinate - index as f64) as f32)
            })
        });
        let rows = self
            .rows
            .iter()
            .map(|row| {
                [
                    row[0] as f32 * 255.0,
                    row[1] as f32 * 255.0,
                    row[2] as f32 * 255.0,
                    0.0,
                ]
            })
            .collect::<Vec<_>>();
        CompiledLut {
            exact: rows
                .iter()
                .flatten()
                .any(|value| !value.is_finite() || value.abs() > 4096.0)
                .then(|| self.clone()),
            rows,
            axes,
            size: self.size,
            channels: self
                .one_d
                .then(|| std::array::from_fn(|v| self.rgb([v as u8; 3]))),
        }
    }
}

fn triple(fields: &[&str]) -> Result<[f64; 3], String> {
    if fields.len() != 3 {
        return Err("expected three numbers".into());
    }
    let mut result = [0.0_f64; 3];
    for (dest, field) in result.iter_mut().zip(fields) {
        *dest = field
            .parse()
            .map_err(|_| format!("invalid number: {field}"))?;
        if !dest.is_finite() {
            return Err("non-finite number".into());
        }
    }
    Ok(result)
}

pub struct CompiledLut {
    rows: Vec<[f32; 4]>,
    axes: [[(usize, f32); 256]; 3],
    size: usize,
    channels: Option<[[u8; 3]; 256]>,
    exact: Option<Lut>,
}

#[inline(always)]
fn interpolate(a: [f32; 4], b: [f32; 4], t: f32) -> [f32; 4] {
    std::array::from_fn(|c| a[c] + (b[c] - a[c]) * t)
}

#[inline(always)]
fn quantize(value: [f32; 4]) -> [u8; 4] {
    value.map(|v| (v + 0.5) as u8)
}

impl CompiledLut {
    pub fn apply(&self, image: Arc<FrameImage>) -> Arc<FrameImage> {
        let mut image = (*image).clone();
        if let Some(channels) = &self.channels {
            for pixel in image.pixels.as_chunks_mut::<4>().0 {
                pixel[0] = channels[pixel[0] as usize][0];
                pixel[1] = channels[pixel[1] as usize][1];
                pixel[2] = channels[pixel[2] as usize][2];
            }
            return Arc::new(image);
        }
        if let Some(exact) = &self.exact {
            for pixel in image.pixels.as_chunks_mut::<4>().0 {
                let rgb = exact.rgb([pixel[0], pixel[1], pixel[2]]);
                pixel[..3].copy_from_slice(&rgb);
            }
            return Arc::new(image);
        }
        for pixel in image.pixels.as_chunks_mut::<4>().0 {
            let (r, rt) = self.axes[0][pixel[0] as usize];
            let (g, gt) = self.axes[1][pixel[1] as usize];
            let (b, bt) = self.axes[2][pixel[2] as usize];
            let index = r + self.size * (g + self.size * b);
            let plane = |index| {
                interpolate(
                    interpolate(self.rows[index], self.rows[index + 1], rt),
                    interpolate(
                        self.rows[index + self.size],
                        self.rows[index + self.size + 1],
                        rt,
                    ),
                    gt,
                )
            };
            let value = interpolate(plane(index), plane(index + self.size * self.size), bt);
            pixel[..3].copy_from_slice(&quantize(value)[..3]);
        }
        Arc::new(image)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cube(size: usize, invert: bool) -> String {
        let mut text = format!("# test\nTITLE \"Example\"\n\nLUT_3D_SIZE {size}\n");
        for b in 0..size {
            for g in 0..size {
                for r in 0..size {
                    for value in [r, g, b] {
                        let v = value as f64 / (size - 1) as f64;
                        text.push_str(&format!("{} ", if invert { 1.0 - v } else { v }));
                    }
                    text.push('\n');
                }
            }
        }
        text
    }

    #[test]
    fn lattice_matches_exact_sampled_nonlinear_luts() {
        let mut text =
            String::from("LUT_3D_SIZE 17\nDOMAIN_MIN -0.1 0.05 0\nDOMAIN_MAX 0.9 1.1 1\n");
        for b in 0..17 {
            for g in 0..17 {
                for r in 0..17 {
                    let [r, g, b] = [r, g, b].map(|v| v as f64 / 16.0);
                    text.push_str(&format!(
                        "{} {} {}\n",
                        r * r + 0.1 * g * b,
                        g.sqrt() - 0.1 * r * b,
                        b * b * b + 0.15 * r * g
                    ));
                }
            }
        }
        for text in [text, "LUT_1D_SIZE 5\nDOMAIN_MIN -0.1 0.05 0\nDOMAIN_MAX 0.9 1.1 1\n-0.1 0 0\n0.02 0.2 0.4\n0.25 0.6 0.65\n0.7 0.8 0.85\n1.1 1 1\n".into()] {
            let exact = Lut::parse(&text).unwrap();
            let mut pixels = Vec::new();
            for b in (0..=255).step_by(15) { for g in (0..=255).step_by(15) { for r in (0..=255).step_by(15) { pixels.extend([r, g, b, 137]); }}}
            let image = Arc::new(FrameImage { width: (pixels.len() / 4) as u32, height: 1, pixels });
            let result = exact.compile().apply(image.clone());
            for (src, dest) in image.pixels.as_chunks::<4>().0.iter().zip(result.pixels.as_chunks::<4>().0) {
                let expected = exact.rgb([src[0], src[1], src[2]]);
                for c in 0..3 { assert!(dest[c].abs_diff(expected[c]) <= 1, "{src:?}: {dest:?} vs {expected:?}"); }
                assert_eq!(dest[3], src[3]);
            }
        }
    }

    #[test]
    fn large_lattice_values_use_exact_interpolation() {
        let text = cube(2, false).replace("0 0 0", "1000000000 -1000000000 0");
        let exact = Lut::parse(&text).unwrap();
        let compiled = exact.compile();
        assert!(compiled.exact.is_some());
        let image = Arc::new(FrameImage {
            width: 1,
            height: 1,
            pixels: vec![254, 254, 254, 137],
        });
        let result = compiled.apply(image);
        assert_eq!(&result.pixels[..3], &exact.rgb([254; 3]));
        assert_eq!(result.pixels[3], 137);
    }

    #[test]
    fn identity_and_interpolation() {
        assert!(Lut::parse(&cube(2, false).replace("Example", "Example #1")).is_ok());
        let identity = Lut::parse(&cube(2, false)).unwrap();
        let invert = Lut::parse(&cube(2, true)).unwrap();
        for rgb in [[0, 0, 0], [255, 255, 255], [128, 64, 200], [255, 0, 64]] {
            assert_eq!(identity.rgb(rgb), rgb);
            assert_eq!(invert.rgb(rgb), rgb.map(|v| 255 - v));
        }
        let nonlinear =
            Lut::parse("LUT_3D_SIZE 2\n0 0 0\n0 0 0\n0 0 0\n0 0 0\n0 0 0\n0 0 0\n0 0 0\n1 1 1\n")
                .unwrap();
        assert_eq!(nonlinear.rgb([128; 3]), [32; 3]);
        let image = Arc::new(FrameImage {
            width: 2,
            height: 1,
            pixels: vec![128, 64, 200, 137, 255, 0, 64, 0],
        });
        assert_eq!(identity.compile().apply(image.clone()).pixels, image.pixels);
    }

    #[test]
    fn one_dimensional_domain_and_clamp() {
        let lut = Lut::parse(
            "LUT_1D_SIZE 2\nDOMAIN_MIN 0.25 0 0\nDOMAIN_MAX 0.75 1 1\n0 1 -1\n1 0 2 # end\n",
        )
        .unwrap();
        assert_eq!(lut.rgb([0, 0, 0]), [0, 255, 0]);
        assert_eq!(lut.rgb([255, 255, 255]), [255, 0, 255]);
        assert_eq!(lut.rgb([128, 128, 128]), [129, 127, 129]);
    }

    #[test]
    fn malformed_files() {
        for (text, message) in [
            ("", "missing LUT size"),
            ("0 0 0", "missing LUT size"),
            ("LUT_3D_SIZE 1", "range"),
            ("LUT_3D_SIZE 257", "range"),
            ("LUT_1D_SIZE 0", "range"),
            ("LUT_3D_SIZE nope", "invalid LUT size"),
            ("LUT_3D_SIZE 2 3", "expected one"),
            ("LUT_3D_SIZE 2\nLUT_1D_SIZE 2", "mixed"),
            ("LUT_1D_SIZE 2\nLUT_1D_SIZE 2", "duplicate"),
            ("LUT_3D_SIZE 2\n0 0 0", "expected 8"),
            ("LUT_1D_SIZE 2\n0 0 0\n1 1 1\n1 1 1", "too many"),
            ("LUT_1D_SIZE 2\nNaN 0 0", "non-finite"),
            ("LUT_1D_SIZE 2\ninf 0 0", "non-finite"),
            ("LUT_1D_SIZE 2\nx 0 0", "invalid number"),
            ("LUT_1D_SIZE 2\n0 0", "three numbers"),
            (
                "LUT_1D_SIZE 2\nDOMAIN_MIN 1 0 0\n0 0 0\n1 1 1",
                "invalid domain",
            ),
            ("LUT_1D_SIZE 2\nDOMAIN_MAX NaN 1 1", "non-finite"),
            ("LUT_1D_SIZE 2\nDOMAIN_MAX 1 1", "three numbers"),
            (
                "LUT_1D_SIZE 2\nDOMAIN_MIN 0 0 0\nDOMAIN_MIN 0 0 0",
                "duplicate",
            ),
            ("LUT_1D_SIZE 2\n0 0 0\nDOMAIN_MIN 0 0 0", "misplaced"),
            ("TITLE unquoted", "quoted"),
        ] {
            let error = Lut::parse(text).err().expect(text);
            assert!(error.contains(message), "{text}: {error}");
        }
    }

    #[test]
    #[ignore = "release per-frame benchmark"]
    fn benchmark_1080p() {
        let text = cube(33, true);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("benchmark.cube");
        std::fs::write(&path, text).unwrap();
        let image = Arc::new(FrameImage {
            width: 1920,
            height: 1080,
            pixels: (0..1920 * 1080)
                .flat_map(|i| {
                    [
                        (i % 256) as u8,
                        (i / 256 % 256) as u8,
                        (i / 65536 % 256) as u8,
                        137,
                    ]
                })
                .collect(),
        });
        let first = std::time::Instant::now();
        let lut = Lut::load(&path).unwrap().compile();
        std::hint::black_box(lut.apply(image.clone()));
        println!(
            "33³ LUT load+prepare+first 1080p frame: {:.3} ms",
            first.elapsed().as_secs_f64() * 1000.0
        );
        let start = std::time::Instant::now();
        for _ in 0..30 {
            let result = std::hint::black_box(lut.apply(image.clone()));
            assert_eq!(result.pixels[3], 137);
        }
        println!(
            "33³ LUT 1920x1080: {:.3} ms/frame (30 frames)",
            start.elapsed().as_secs_f64() * 1000.0 / 30.0
        );
    }
}
