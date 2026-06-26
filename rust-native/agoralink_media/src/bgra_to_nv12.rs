use crate::color_spec::{ColorMatrix, ColorSpec};

pub fn buffer_size(width: u32, height: u32) -> Result<usize, String> {
    if width == 0 || height == 0 || width % 2 != 0 || height % 2 != 0 {
        return Err("BGRA to NV12 requires non-zero even width and height".to_string());
    }
    let pixels = (width as usize)
        .checked_mul(height as usize)
        .ok_or_else(|| "NV12 dimensions overflow".to_string())?;
    pixels
        .checked_add(pixels / 2)
        .ok_or_else(|| "NV12 buffer size overflow".to_string())
}

pub fn convert(
    bgra: &[u8],
    row_pitch: usize,
    width: u32,
    height: u32,
    nv12: &mut [u8],
) -> Result<(), String> {
    convert_with_spec(bgra, row_pitch, width, height, nv12, ColorSpec::default())
}

pub fn convert_with_spec(
    bgra: &[u8],
    row_pitch: usize,
    width: u32,
    height: u32,
    nv12: &mut [u8],
    color: ColorSpec,
) -> Result<(), String> {
    convert_scaled_with_spec(bgra, row_pitch, width, height, width, height, nv12, color)
}

pub fn convert_scaled(
    bgra: &[u8],
    row_pitch: usize,
    source_width: u32,
    source_height: u32,
    output_width: u32,
    output_height: u32,
    nv12: &mut [u8],
) -> Result<(), String> {
    convert_scaled_with_spec(
        bgra,
        row_pitch,
        source_width,
        source_height,
        output_width,
        output_height,
        nv12,
        ColorSpec::default(),
    )
}

#[allow(clippy::too_many_arguments)]
pub fn convert_scaled_with_spec(
    bgra: &[u8],
    row_pitch: usize,
    source_width: u32,
    source_height: u32,
    output_width: u32,
    output_height: u32,
    nv12: &mut [u8],
    color: ColorSpec,
) -> Result<(), String> {
    let output_size = buffer_size(output_width, output_height)?;
    if nv12.len() != output_size {
        return Err(format!(
            "NV12 output length mismatch: expected {output_size}, got {}",
            nv12.len()
        ));
    }
    let y_stride = output_width as usize;
    let uv_stride = output_width as usize;
    let uv_offset = y_stride
        .checked_mul(output_height as usize)
        .ok_or_else(|| "NV12 UV offset overflow".to_string())?;
    convert_scaled_with_layout(
        bgra,
        row_pitch,
        source_width,
        source_height,
        output_width,
        output_height,
        y_stride,
        uv_stride,
        uv_offset,
        nv12,
        color,
    )
}

#[allow(clippy::too_many_arguments)]
pub fn convert_scaled_with_layout(
    bgra: &[u8],
    source_stride: usize,
    source_width: u32,
    source_height: u32,
    output_width: u32,
    output_height: u32,
    y_stride: usize,
    uv_stride: usize,
    uv_offset: usize,
    nv12: &mut [u8],
    color: ColorSpec,
) -> Result<(), String> {
    validate_layout(
        bgra,
        source_stride,
        source_width,
        source_height,
        output_width,
        output_height,
        y_stride,
        uv_stride,
        uv_offset,
        nv12,
    )?;

    let output_width = output_width as usize;
    let output_height = output_height as usize;
    let x_map = build_axis_map(source_width as usize, output_width);
    let y_map = build_axis_map(source_height as usize, output_height);

    for output_y in (0..output_height).step_by(2) {
        let y_row0 = output_y * y_stride;
        let y_row1 = (output_y + 1) * y_stride;
        let uv_row = uv_offset + (output_y / 2) * uv_stride;
        for output_x in (0..output_width).step_by(2) {
            let pixels = [
                sample_bilinear(bgra, source_stride, x_map[output_x], y_map[output_y]),
                sample_bilinear(bgra, source_stride, x_map[output_x + 1], y_map[output_y]),
                sample_bilinear(bgra, source_stride, x_map[output_x], y_map[output_y + 1]),
                sample_bilinear(
                    bgra,
                    source_stride,
                    x_map[output_x + 1],
                    y_map[output_y + 1],
                ),
            ];

            nv12[y_row0 + output_x] = rgb_to_y(pixels[0], color);
            nv12[y_row0 + output_x + 1] = rgb_to_y(pixels[1], color);
            nv12[y_row1 + output_x] = rgb_to_y(pixels[2], color);
            nv12[y_row1 + output_x + 1] = rgb_to_y(pixels[3], color);

            let mut u_sum = 0u32;
            let mut v_sum = 0u32;
            for pixel in pixels {
                let (u, v) = rgb_to_uv(pixel, color);
                u_sum += u32::from(u);
                v_sum += u32::from(v);
            }
            nv12[uv_row + output_x] = ((u_sum + 2) / 4) as u8;
            nv12[uv_row + output_x + 1] = ((v_sum + 2) / 4) as u8;
        }
    }
    Ok(())
}

#[derive(Clone, Copy)]
struct AxisSample {
    first: usize,
    second: usize,
    fraction: u32,
}

fn build_axis_map(source_len: usize, output_len: usize) -> Vec<AxisSample> {
    if output_len <= 1 || source_len <= 1 {
        return vec![
            AxisSample {
                first: 0,
                second: 0,
                fraction: 0,
            };
            output_len
        ];
    }
    (0..output_len)
        .map(|output| {
            let fixed = ((output as u64 * (source_len - 1) as u64) << 16) / (output_len - 1) as u64;
            let first = (fixed >> 16) as usize;
            AxisSample {
                first,
                second: (first + 1).min(source_len - 1),
                fraction: (fixed & 0xffff) as u32,
            }
        })
        .collect()
}

fn sample_bilinear(bgra: &[u8], stride: usize, x: AxisSample, y: AxisSample) -> (u8, u8, u8) {
    let p00 = pixel(bgra, stride, x.first, y.first);
    let p01 = pixel(bgra, stride, x.second, y.first);
    let p10 = pixel(bgra, stride, x.first, y.second);
    let p11 = pixel(bgra, stride, x.second, y.second);
    (
        bilinear_channel(p00.0, p01.0, p10.0, p11.0, x.fraction, y.fraction),
        bilinear_channel(p00.1, p01.1, p10.1, p11.1, x.fraction, y.fraction),
        bilinear_channel(p00.2, p01.2, p10.2, p11.2, x.fraction, y.fraction),
    )
}

fn bilinear_channel(p00: u8, p01: u8, p10: u8, p11: u8, fx: u32, fy: u32) -> u8 {
    let top = u64::from(p00) * u64::from(65_536 - fx) + u64::from(p01) * u64::from(fx);
    let bottom = u64::from(p10) * u64::from(65_536 - fx) + u64::from(p11) * u64::from(fx);
    let value = top * u64::from(65_536 - fy) + bottom * u64::from(fy);
    ((value + (1 << 31)) >> 32) as u8
}

#[inline(always)]
fn pixel(bgra: &[u8], stride: usize, x: usize, y: usize) -> (u8, u8, u8) {
    let offset = y * stride + x * 4;
    (bgra[offset], bgra[offset + 1], bgra[offset + 2])
}

#[inline(always)]
fn rgb_to_y((blue, green, red): (u8, u8, u8), color: ColorSpec) -> u8 {
    let (r, g, b) = (i32::from(red), i32::from(green), i32::from(blue));
    let value = match color.matrix {
        ColorMatrix::Bt601 => 66 * r + 129 * g + 25 * b,
        ColorMatrix::Bt709 => 47 * r + 157 * g + 16 * b,
    };
    (((value + 128) >> 8) + 16).clamp(16, 235) as u8
}

#[inline(always)]
fn rgb_to_uv((blue, green, red): (u8, u8, u8), color: ColorSpec) -> (u8, u8) {
    let (r, g, b) = (i32::from(red), i32::from(green), i32::from(blue));
    let (u_value, v_value) = match color.matrix {
        ColorMatrix::Bt601 => (-38 * r - 74 * g + 112 * b, 112 * r - 94 * g - 18 * b),
        ColorMatrix::Bt709 => (-26 * r - 86 * g + 112 * b, 112 * r - 102 * g - 10 * b),
    };
    let u = (((u_value + 128) >> 8) + 128).clamp(16, 240) as u8;
    let v = (((v_value + 128) >> 8) + 128).clamp(16, 240) as u8;
    (u, v)
}

#[allow(clippy::too_many_arguments)]
fn validate_layout(
    bgra: &[u8],
    source_stride: usize,
    source_width: u32,
    source_height: u32,
    output_width: u32,
    output_height: u32,
    y_stride: usize,
    uv_stride: usize,
    uv_offset: usize,
    nv12: &[u8],
) -> Result<(), String> {
    buffer_size(output_width, output_height)?;
    if source_width == 0 || source_height == 0 {
        return Err("source dimensions must be non-zero".to_string());
    }
    let source_row_bytes = source_width as usize * 4;
    if source_stride < source_row_bytes {
        return Err(format!(
            "BGRA source stride {source_stride} is smaller than row bytes {source_row_bytes}"
        ));
    }
    let required_input = source_stride
        .checked_mul(source_height as usize)
        .ok_or_else(|| "BGRA input size overflow".to_string())?;
    if bgra.len() < required_input {
        return Err(format!(
            "BGRA input too short: need {required_input}, got {}",
            bgra.len()
        ));
    }
    if y_stride < output_width as usize || uv_stride < output_width as usize {
        return Err("NV12 destination stride is smaller than output width".to_string());
    }
    let visible_y_end = y_stride
        .checked_mul(output_height as usize)
        .ok_or_else(|| "NV12 visible Y size overflow".to_string())?;
    if uv_offset < visible_y_end {
        return Err("NV12 UV offset overlaps visible Y plane".to_string());
    }
    let required_output = uv_offset
        .checked_add(
            uv_stride
                .checked_mul(output_height as usize / 2)
                .ok_or_else(|| "NV12 UV size overflow".to_string())?,
        )
        .ok_or_else(|| "NV12 output size overflow".to_string())?;
    if nv12.len() < required_output {
        return Err(format!(
            "NV12 output too short: need {required_output}, got {}",
            nv12.len()
        ));
    }
    Ok(())
}

pub fn run_self_test() -> Result<(), String> {
    if buffer_size(2, 2)? != 6 || buffer_size(3, 2).is_ok() || buffer_size(2, 3).is_ok() {
        return Err("NV12 dimension validation failed".to_string());
    }

    let black = solid_bgra(2, 2, (0, 0, 0));
    let mut nv12 = vec![0u8; 6];
    convert(&black, 8, 2, 2, &mut nv12)?;
    if nv12 != [16, 16, 16, 16, 128, 128] {
        return Err(format!("unexpected Rec.709 black NV12: {nv12:?}"));
    }
    let white = solid_bgra(2, 2, (255, 255, 255));
    convert(&white, 8, 2, 2, &mut nv12)?;
    if nv12 != [235, 235, 235, 235, 128, 128] {
        return Err(format!("unexpected Rec.709 white NV12: {nv12:?}"));
    }

    let blue = solid_bgra(2, 2, (255, 0, 0));
    convert(&blue, 8, 2, 2, &mut nv12)?;
    if nv12[4] <= nv12[5] {
        return Err(format!("NV12 UV order is not U,V for blue: {nv12:?}"));
    }
    let red = solid_bgra(2, 2, (0, 0, 255));
    convert(&red, 8, 2, 2, &mut nv12)?;
    if nv12[5] <= nv12[4] {
        return Err(format!("NV12 UV order is not U,V for red: {nv12:?}"));
    }

    let source = color_grid_bgra(4, 4);
    let mut scaled = vec![0u8; buffer_size(2, 2)?];
    convert_scaled(&source, 16, 4, 4, 2, 2, &mut scaled)?;
    if scaled
        .iter()
        .take(4)
        .any(|value| !(16..=235).contains(value))
        || scaled
            .iter()
            .skip(4)
            .any(|value| !(16..=240).contains(value))
    {
        return Err("scaled Rec.709 NV12 values left limited range".to_string());
    }

    let mut padded = vec![0u8; 16];
    convert_scaled_with_layout(
        &black,
        8,
        2,
        2,
        2,
        2,
        4,
        4,
        8,
        &mut padded,
        ColorSpec::default(),
    )?;
    if padded[0] != 16 || padded[4] != 16 || padded[8] != 128 || padded[9] != 128 {
        return Err("explicit destination stride conversion failed".to_string());
    }
    Ok(())
}

fn solid_bgra(width: usize, height: usize, bgr: (u8, u8, u8)) -> Vec<u8> {
    let mut output = Vec::with_capacity(width * height * 4);
    for _ in 0..width * height {
        output.extend_from_slice(&[bgr.0, bgr.1, bgr.2, 255]);
    }
    output
}

fn color_grid_bgra(width: usize, height: usize) -> Vec<u8> {
    let colors = [(0, 0, 0), (255, 255, 255), (0, 0, 255), (255, 0, 0)];
    let mut output = Vec::with_capacity(width * height * 4);
    for y in 0..height {
        for x in 0..width {
            let color = colors[(x + y) % colors.len()];
            output.extend_from_slice(&[color.0, color.1, color.2, 255]);
        }
    }
    output
}
