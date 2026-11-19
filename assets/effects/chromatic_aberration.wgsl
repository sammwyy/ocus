fn effect(uv: vec2<f32>) -> vec4<f32> {
    let c = sample(uv);
    let offset = vec2<f32>(params.amount * 8.0 / params.size.x, 0.0);
    return vec4<f32>(sample(uv + offset).r, c.g, sample(uv - offset).b, c.a);
}
