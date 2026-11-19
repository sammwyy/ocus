# WGSL clip effects

Select a video or image clip and open **Inspector → Effect**. Choose a built-in
or **Load .wgsl…**, adjust Amount, or clear the effect. Each clip has one effect;
titles and audio clips cannot have one. Amount edits undo as one drag. Splitting
and copying preserve the effect. Preview and export use the same compositor:
**chroma key → color adjustments → LUT → effect → transform → transitions/layer blending**.
Source preview remains ungraded.

## Shader contract

A UTF-8 WGSL file (at most 1 MiB) defines:

```wgsl
fn effect(uv: vec2<f32>) -> vec4<f32> {
    let color = sample(uv);
    return vec4<f32>(mix(color.rgb, 1.0 - color.rgb, params.amount), color.a);
}
```

Ocus prepends a prelude and appends a compute entry point. Do not declare your
own bindings or entry point; names beginning `ocus_` are reserved. Available:

- `sample(uv: vec2<f32>) -> vec4<f32>`: nearest-neighbor, edge-clamped sampling.
- `pixel(xy: vec2<i32>) -> vec4<f32>`: edge-clamped integer pixel sampling.
- `params.size: vec2<f32>`: dimensions of this rendered frame, in pixels.
- `params.amount: f32`: finite value in `[0, 1]`, default 1. The shader decides
  how to use it. Built-in Blur maps it to a box radius of 0–8 rendered pixels.
- `params.time: f32`: timeline seconds elapsed since this clip's start (not
  source time, so reverse/freeze still advance it).

Coordinates have origin at the top left; the entry point supplies pixel-center
UVs. Colors are straight (not premultiplied) RGBA in `[0, 1]`, in the frame's
existing encoded color space, not linear light. The output is clamped and packed
to RGBA8. Alpha may be changed. Amount zero is not automatically bypassed:
custom effects own its meaning. The seven built-ins in `assets/effects/` use
this exact contract via `include_str!`, forming the base for future plugins.

## Execution and persistence

`media::effects` lazily creates one process-wide headless wgpu device, trying
all backends and then a fallback adapter. Composition and file validation run
on preview, storyboard or export workers, never the UI thread. Compiled pipelines
are cached (up to 32) by built-in ID or file path, modification time and length.
Naga validates the wrapped source before compilation. A missing adapter or invalid
shader leaves the clip unmodified at the effect stage; the Inspector shows the
error. The file picker reports validation errors before changing the project.
Only load shaders you trust: WGSL validation is not a sandbox for arbitrary GPU
workloads, and expensive shaders can make preview and export slow.

The optional `effect` field is compatible with version 1 projects. It stores
`shader: {"builtin": "grayscale"}` or `shader: {"file": "look.wgsl"}` and
`amount`. Relative shader paths resolve beside the `.ocus` project when opened.
Shader files are referenced, not copied or embedded. OTIO/EDL do not store
effects; Ocus adds an omitted-settings note to those exports.
