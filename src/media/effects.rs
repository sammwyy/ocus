use super::preview::FrameImage;
use crate::timeline::{Effect, EffectShader};
use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, Mutex, OnceLock};
use std::task::{Context, Poll, Wake, Waker};
use wgpu::util::DeviceExt;

pub const BUILTINS: &[(&str, &str)] = &[
    ("grayscale", "Grayscale"),
    ("sepia", "Sepia"),
    ("invert", "Invert"),
    ("vignette", "Vignette"),
    ("blur", "Blur"),
    ("pixelate", "Pixelate"),
    ("chromatic_aberration", "Chromatic aberration"),
];

const PRELUDE: &str = r#"
struct Params { size: vec2<f32>, amount: f32, time: f32 }
@group(0) @binding(0) var<storage, read> ocus_input: array<u32>;
@group(0) @binding(1) var<storage, read_write> ocus_output: array<u32>;
@group(0) @binding(2) var<uniform> params: Params;
fn pixel(xy: vec2<i32>) -> vec4<f32> {
    let p = vec2<u32>(clamp(xy, vec2<i32>(0), vec2<i32>(params.size) - vec2<i32>(1)));
    return unpack4x8unorm(ocus_input[p.y * u32(params.size.x) + p.x]);
}
fn sample(uv: vec2<f32>) -> vec4<f32> {
    return pixel(vec2<i32>(floor(clamp(uv, vec2<f32>(0.0), vec2<f32>(1.0)) * params.size)));
}
"#;

const WRAPPER: &str = r#"
@compute @workgroup_size(8, 8)
fn ocus_main(@builtin(global_invocation_id) id: vec3<u32>) {
    if (id.x >= u32(params.size.x) || id.y >= u32(params.size.y)) { return; }
    let uv = (vec2<f32>(id.xy) + vec2<f32>(0.5)) / params.size;
    ocus_output[id.y * u32(params.size.x) + id.x] = pack4x8unorm(clamp(effect(uv), vec4<f32>(0.0), vec4<f32>(1.0)));
}
"#;

fn wrapped(source: &str) -> String {
    format!("{PRELUDE}\n{source}\n{WRAPPER}")
}

fn validate_source(source: &str) -> Result<(), String> {
    let text = wrapped(source);
    let module =
        wgpu::naga::front::wgsl::parse_str(&text).map_err(|error| error.emit_to_string(&text))?;
    wgpu::naga::valid::Validator::new(
        wgpu::naga::valid::ValidationFlags::all(),
        wgpu::naga::valid::Capabilities::empty(),
    )
    .validate(&module)
    .map_err(|error| error.emit_to_string(&text))?;
    Ok(())
}

fn shader_key(shader: &EffectShader) -> Result<String, String> {
    match shader {
        EffectShader::Builtin(id) => Ok(format!("builtin:{id}")),
        EffectShader::File(path) => {
            let metadata = path
                .metadata()
                .map_err(|error| format!("{}: {error}", path.display()))?;
            if metadata.len() > 1024 * 1024 {
                return Err("Effect shader exceeds 1 MiB".into());
            }
            Ok(format!(
                "file:{}:{:?}:{}",
                path.display(),
                metadata.modified(),
                metadata.len()
            ))
        }
    }
}

fn source(shader: &EffectShader) -> Result<String, String> {
    match shader {
        EffectShader::Builtin(id) => {
            let text = match id.as_str() {
                "grayscale" => include_str!("../../assets/effects/grayscale.wgsl"),
                "sepia" => include_str!("../../assets/effects/sepia.wgsl"),
                "invert" => include_str!("../../assets/effects/invert.wgsl"),
                "vignette" => include_str!("../../assets/effects/vignette.wgsl"),
                "blur" => include_str!("../../assets/effects/blur.wgsl"),
                "pixelate" => include_str!("../../assets/effects/pixelate.wgsl"),
                "chromatic_aberration" => {
                    include_str!("../../assets/effects/chromatic_aberration.wgsl")
                }
                _ => return Err(format!("Unknown effect: {id}")),
            };
            Ok(text.into())
        }
        EffectShader::File(path) => {
            std::fs::read_to_string(path).map_err(|error| format!("{}: {error}", path.display()))
        }
    }
}

struct ThreadWake(std::thread::Thread);
impl Wake for ThreadWake {
    fn wake(self: Arc<Self>) {
        self.0.unpark();
    }
    fn wake_by_ref(self: &Arc<Self>) {
        self.0.unpark();
    }
}

fn block_on<T>(future: impl Future<Output = T>) -> T {
    let waker = Waker::from(Arc::new(ThreadWake(std::thread::current())));
    let mut context = Context::from_waker(&waker);
    let mut future = std::pin::pin!(future);
    loop {
        match future.as_mut().poll(&mut context) {
            Poll::Ready(value) => return value,
            Poll::Pending => std::thread::park(),
        }
    }
}

struct Engine {
    device: wgpu::Device,
    queue: wgpu::Queue,
    pipelines: HashMap<String, Result<wgpu::ComputePipeline, String>>,
}

impl Engine {
    fn new() -> Result<Self, String> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::all(),
            ..Default::default()
        });
        let options = wgpu::RequestAdapterOptions::default();
        let adapter = block_on(instance.request_adapter(&options))
            .or_else(|| {
                block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
                    force_fallback_adapter: true,
                    ..options
                }))
            })
            .ok_or("No GPU adapter is available for effects")?;
        let (device, queue) = block_on(adapter.request_device(
            &wgpu::DeviceDescriptor {
                label: Some("Ocus effects"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::default(),
                memory_hints: wgpu::MemoryHints::MemoryUsage,
            },
            None,
        ))
        .map_err(|error| error.to_string())?;
        Ok(Self {
            device,
            queue,
            pipelines: HashMap::new(),
        })
    }

    fn compile(
        &mut self,
        key: String,
        load_source: impl FnOnce() -> Result<String, String>,
    ) -> Result<&wgpu::ComputePipeline, String> {
        if !self.pipelines.contains_key(&key) {
            let source = load_source()?;
            if self.pipelines.len() >= 32 {
                self.pipelines.clear();
            }
            if let Err(error) = validate_source(&source) {
                self.pipelines.insert(key, Err(error.clone()));
                return Err(error);
            }
            self.device.push_error_scope(wgpu::ErrorFilter::Validation);
            let module = self
                .device
                .create_shader_module(wgpu::ShaderModuleDescriptor {
                    label: Some("Effect WGSL"),
                    source: wgpu::ShaderSource::Wgsl(wrapped(&source).into()),
                });
            let entries = [
                (0, wgpu::BufferBindingType::Storage { read_only: true }),
                (1, wgpu::BufferBindingType::Storage { read_only: false }),
                (2, wgpu::BufferBindingType::Uniform),
            ]
            .map(|(binding, ty)| wgpu::BindGroupLayoutEntry {
                binding,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::Buffer {
                    ty,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            });
            let bindings = self
                .device
                .create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                    label: None,
                    entries: &entries,
                });
            let layout = self
                .device
                .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                    label: None,
                    bind_group_layouts: &[&bindings],
                    push_constant_ranges: &[],
                });
            let pipeline = self
                .device
                .create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                    label: Some("Effect"),
                    layout: Some(&layout),
                    module: &module,
                    entry_point: "ocus_main",
                    compilation_options: Default::default(),
                    cache: None,
                });
            let result = match block_on(self.device.pop_error_scope()) {
                Some(error) => Err(format!("Effect shader: {error}")),
                None => Ok(pipeline),
            };
            self.pipelines.insert(key.clone(), result);
        }
        self.pipelines
            .get(&key)
            .unwrap()
            .as_ref()
            .map_err(Clone::clone)
    }

    fn render(
        &mut self,
        effect: &Effect,
        image: &FrameImage,
        time: f32,
    ) -> Result<FrameImage, String> {
        let key = shader_key(&effect.shader)?;
        self.compile(key.clone(), || source(&effect.shader))?;
        let size = image.pixels.len() as u64;
        if image.width == 0
            || image.height == 0
            || size != u64::from(image.width) * u64::from(image.height) * 4
            || size > u64::from(self.device.limits().max_storage_buffer_binding_size)
        {
            return Err("Effect frame size is unsupported".into());
        }
        let input = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: None,
                contents: &image.pixels,
                usage: wgpu::BufferUsages::STORAGE,
            });
        let output = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        let values = [image.width as f32, image.height as f32, effect.amount, time];
        let bytes: Vec<u8> = values.into_iter().flat_map(f32::to_le_bytes).collect();
        let params = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: None,
                contents: &bytes,
                usage: wgpu::BufferUsages::UNIFORM,
            });
        let pipeline = self.pipelines[&key].as_ref().map_err(Clone::clone)?;
        let group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &pipeline.get_bind_group_layout(0),
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: input.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: output.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: params.as_entire_binding(),
                },
            ],
        });
        let readback = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let mut encoder = self.device.create_command_encoder(&Default::default());
        {
            let mut pass = encoder.begin_compute_pass(&Default::default());
            pass.set_pipeline(pipeline);
            pass.set_bind_group(0, &group, &[]);
            pass.dispatch_workgroups(image.width.div_ceil(8), image.height.div_ceil(8), 1);
        }
        encoder.copy_buffer_to_buffer(&output, 0, &readback, 0, size);
        self.queue.submit(Some(encoder.finish()));
        let (send, recv) = std::sync::mpsc::sync_channel(1);
        readback
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |result| {
                let _ = send.send(result);
            });
        self.device.poll(wgpu::Maintain::Wait);
        recv.recv()
            .map_err(|error| error.to_string())?
            .map_err(|error| error.to_string())?;
        let pixels = readback.slice(..).get_mapped_range().to_vec();
        readback.unmap();
        Ok(FrameImage {
            pixels,
            picture: image.picture,
            width: image.width,
            height: image.height,
        })
    }
}

static ENGINE: OnceLock<Result<Mutex<Engine>, String>> = OnceLock::new();
static ERRORS: OnceLock<Mutex<HashMap<EffectShader, String>>> = OnceLock::new();

fn engine() -> Result<std::sync::MutexGuard<'static, Engine>, String> {
    ENGINE
        .get_or_init(|| Engine::new().map(Mutex::new))
        .as_ref()
        .map_err(Clone::clone)?
        .lock()
        .map_err(|_| "Effect device is unavailable".into())
}

#[cfg(test)]
pub(crate) fn gpu_available() -> Result<(), String> {
    engine().map(|_| ())
}

pub fn validate(shader: &EffectShader) -> Result<(), String> {
    let key = shader_key(shader)?;
    let source = source(shader)?;
    validate_source(&source)?;
    engine()?.compile(key, || Ok(source)).map(|_| ())
}

pub fn error(shader: &EffectShader) -> Option<String> {
    ERRORS.get()?.lock().ok()?.get(shader).cloned()
}

pub fn apply(effect: Option<&Effect>, image: Arc<FrameImage>, time: f32) -> Arc<FrameImage> {
    let Some(effect) = effect else {
        return image;
    };
    let result = engine().and_then(|mut engine| engine.render(effect, &image, time));
    let mut errors = ERRORS.get_or_init(Default::default).lock().unwrap();
    match result {
        Ok(frame) => {
            errors.remove(&effect.shader);
            Arc::new(frame)
        }
        Err(error) => {
            if errors.len() >= 32 {
                errors.clear();
            }
            errors.insert(effect.shader.clone(), error);
            image
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contract_validates_user_shader_and_all_builtins() {
        validate_source("fn effect(uv: vec2<f32>) -> vec4<f32> { return pixel(vec2<i32>(uv * params.size)) * vec4<f32>(1.0 - params.time * 0.0); }").unwrap();
        for (id, _) in BUILTINS {
            let text = source(&EffectShader::Builtin((*id).into())).unwrap();
            validate_source(&text).unwrap();
        }
        let error =
            validate_source("fn effect(uv: vec2<f32>) -> vec4<f32> { return missing(uv); }")
                .unwrap_err();
        assert!(error.contains("missing"), "{error}");
        assert!(error.contains("error"), "{error}");
        assert!(validate_source("fn effect() -> f32 { return 1.0; }").is_err());
    }

    #[test]
    fn file_pipeline_cache_loads_only_changed_sources() {
        let mut engine = match Engine::new() {
            Ok(engine) => engine,
            Err(error) => {
                eprintln!("Skipping GPU effect cache: {error}");
                return;
            }
        };
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cached.wgsl");
        let shader = EffectShader::File(path.clone());
        let text = "fn effect(uv: vec2<f32>) -> vec4<f32> { return sample(uv) * 0.0; }";
        std::fs::write(&path, text).unwrap();
        let loads = std::cell::Cell::new(0);
        let mut compile = || {
            engine
                .compile(shader_key(&shader)?, || {
                    loads.set(loads.get() + 1);
                    source(&shader)
                })
                .map(|_| ())
        };
        compile().unwrap();
        compile().unwrap();
        assert_eq!(loads.get(), 1);

        let original_key = shader_key(&shader).unwrap();
        let modified = path.metadata().unwrap().modified().unwrap();
        std::fs::write(&path, text.replace("0.0", "1.0")).unwrap();
        std::fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(modified + std::time::Duration::from_secs(2))
            .unwrap();
        assert_ne!(shader_key(&shader).unwrap(), original_key);
        compile().unwrap();
        compile().unwrap();
        assert_eq!(loads.get(), 2);

        std::fs::write(&path, "").unwrap();
        let error = compile().unwrap_err();
        assert!(error.contains("effect"), "{error}");
        assert_eq!(compile().unwrap_err(), error);
        assert_eq!(loads.get(), 3);
        std::fs::remove_file(&path).unwrap();
        assert!(compile().unwrap_err().contains("cached.wgsl"));
        assert_eq!(loads.get(), 3);
    }

    #[test]
    fn gpu_pixels_and_error_fallback() {
        if let Err(error) = engine() {
            eprintln!("Skipping GPU effect pixels: {error}");
            return;
        }
        let image = Arc::new(FrameImage {
            picture: (0, 0, 2, 1),
            width: 2,
            height: 1,
            pixels: vec![255, 0, 0, 128, 20, 80, 160, 255],
        });
        assert!(Arc::ptr_eq(&image, &apply(None, image.clone(), 0.0)));
        for (id, expected) in [
            ("invert", vec![0, 255, 255, 128, 235, 175, 95, 255]),
            ("grayscale", vec![54, 54, 54, 128, 73, 73, 73, 255]),
        ] {
            let effect = Effect {
                shader: EffectShader::Builtin(id.into()),
                amount: 1.0,
            };
            let output = apply(Some(&effect), image.clone(), 0.0);
            assert!(
                error(&effect.shader).is_none(),
                "{:?}",
                error(&effect.shader)
            );
            for (actual, expected) in output.pixels.iter().zip(expected) {
                assert!(
                    i32::from(*actual).abs_diff(expected) <= 1,
                    "{id}: {:?}",
                    output.pixels
                );
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("effect.wgsl");
        std::fs::write(&path, "fn effect(uv: vec2<f32>) -> vec4<f32> { return vec4<f32>(params.time, params.amount, params.size.x / 2.0, 1.0); }").unwrap();
        let effect = Effect {
            shader: EffectShader::File(path.clone()),
            amount: 0.5,
        };
        assert_eq!(
            apply(Some(&effect), image.clone(), 0.0).pixels,
            [0, 128, 255, 255].repeat(2)
        );
        std::fs::write(&path, "broken shader").unwrap();
        assert!(Arc::ptr_eq(
            &image,
            &apply(Some(&effect), image.clone(), 0.0)
        ));
        assert!(error(&effect.shader).unwrap().contains("error"));
        std::fs::remove_file(path).unwrap();
        assert!(Arc::ptr_eq(
            &image,
            &apply(Some(&effect), image.clone(), 0.0)
        ));
        assert!(error(&effect.shader).is_some());
    }
}
