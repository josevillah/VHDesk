// Convierte I420 a RGBA (BT.601, rango limitado a rango completo).
//
// Los tres planos llegan como texturas R8Unorm, o sea normalizadas a [0,1]. Se recupera
// el valor de 8 bits y se aplica la matriz inversa de BT.601. El resultado se recorta a
// [0,255] porque el rango limitado de entrada puede dar valores fuera al descodificar
// (por ejemplo, un Y=16 con croma fuerte produce R/G/B negativos o por encima de 255).

@group(0) @binding(0) var plano_y: texture_2d<f32>;
@group(0) @binding(1) var plano_u: texture_2d<f32>;
@group(0) @binding(2) var plano_v: texture_2d<f32>;
@group(0) @binding(3) var muestreador: sampler;

struct SalidaVertex {
    @builtin(position) posicion: vec4<f32>,
    @location(0) uv: vec2<f32>,
}

@vertex
fn vs_main(@builtin(vertex_index) indice: u32) -> SalidaVertex {
    var salida: SalidaVertex;
    // Triangulo de pantalla completa sin buffer de vertices: los tres vertices cubren todo
    // el viewport con las coordenadas (-1,-1), (3,-1) y (-1,3), y la uv interpola de (0,0)
    // a (1,1) exactamente en los centros de texel.
    let x = f32(i32((indice & 1u) * 4u) - 1);
    let y = f32(i32((indice >> 1u) * 4u) - 1);
    salida.posicion = vec4<f32>(x, y, 0.0, 1.0);
    salida.uv = vec2<f32>((x + 1.0) * 0.5, (1.0 - y) * 0.5);
    return salida;
}

@fragment
fn fs_main(entrada: SalidaVertex) -> @location(0) vec4<f32> {
    let y = textureSample(plano_y, muestreador, entrada.uv).r;
    let u = textureSample(plano_u, muestreador, entrada.uv).r;
    let v = textureSample(plano_v, muestreador, entrada.uv).r;

    // R8Unorm entrega [0,1]; se vuelve al espacio de 8 bits antes de aplicar la matriz.
    let yn = (y * 255.0 - 16.0) * 1.1643835616438356;
    let un = u * 255.0 - 128.0;
    let vn = v * 255.0 - 128.0;

    let r = clamp(yn + 1.5960267857142856 * vn, 0.0, 255.0);
    let g = clamp(yn - 0.3917622900949137 * un - 0.8129676472377708 * vn, 0.0, 255.0);
    let b = clamp(yn + 2.017232142857143 * un, 0.0, 255.0);

    return vec4<f32>(r, g, b, 255.0) / 255.0;
}
