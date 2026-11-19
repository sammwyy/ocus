fn effect(uv: vec2<f32>) -> vec4<f32> {
    let radius = i32(round(params.amount * 8.0));
    let center = vec2<i32>(uv * params.size);
    var c = vec4<f32>(0.0);
    for (var y = -radius; y <= radius; y++) {
        for (var x = -radius; x <= radius; x++) {
            c += pixel(center + vec2<i32>(x, y));
        }
    }
    let side = f32(radius * 2 + 1);
    return c / (side * side);
}
