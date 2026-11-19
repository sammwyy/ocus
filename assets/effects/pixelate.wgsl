fn effect(uv: vec2<f32>) -> vec4<f32> {
    let block = max(1.0, round(params.amount * 32.0));
    let xy = (floor(uv * params.size / block) + 0.5) * block;
    return sample(xy / params.size);
}
