fn effect(uv: vec2<f32>) -> vec4<f32> {
    let c = sample(uv);
    let d = length((uv - vec2<f32>(0.5)) * 1.41421356);
    return vec4<f32>(c.rgb * (1.0 - params.amount * smoothstep(0.25, 1.0, d)), c.a);
}
