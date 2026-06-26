use crate::color_spec::{ColorMatrix, ColorSpec};

pub fn convert(nv12: &[u8], width: u32, height: u32, bgra: &mut Vec<u8>) -> Result<(), String> {
    convert_with_strides(nv12, width, height, width as usize, width as usize, bgra)
}

pub fn convert_with_strides(
    nv12: &[u8],
    width: u32,
    height: u32,
    y_stride: usize,
    uv_stride: usize,
    bgra: &mut Vec<u8>,
) -> Result<(), String> {
    let uv_offset = y_stride
        .checked_mul(height as usize)
        .ok_or_else(|| "NV12 UV offset overflow".to_string())?;
    convert_with_layout(nv12, width, height, y_stride, uv_stride, uv_offset, bgra)
}

pub fn convert_with_layout(
    nv12: &[u8],
    width: u32,
    height: u32,
    y_stride: usize,
    uv_stride: usize,
    uv_offset: usize,
    bgra: &mut Vec<u8>,
) -> Result<(), String> {
    convert_with_layout_and_spec(
        nv12,
        width,
        height,
        y_stride,
        uv_stride,
        uv_offset,
        bgra,
        ColorSpec::default(),
    )
}

#[allow(clippy::too_many_arguments)]
pub fn convert_with_layout_and_spec(
    nv12: &[u8],
    width: u32,
    height: u32,
    y_stride: usize,
    uv_stride: usize,
    uv_offset: usize,
    bgra: &mut Vec<u8>,
    color: ColorSpec,
) -> Result<(), String> {
    if width == 0 || height == 0 || width % 2 != 0 || height % 2 != 0 {
        return Err("NV12 width and height must be non-zero even values".to_string());
    }
    let width = width as usize;
    let height = height as usize;
    if y_stride < width || uv_stride < width {
        return Err(format!(
            "NV12 stride is smaller than width: width={width}, y_stride={y_stride}, uv_stride={uv_stride}"
        ));
    }
    let visible_y_size = y_stride
        .checked_mul(height)
        .ok_or_else(|| "NV12 Y plane size overflow".to_string())?;
    if uv_offset < visible_y_size {
        return Err(format!(
            "NV12 UV offset overlaps visible Y plane: uv_offset={uv_offset}, visible_y_size={visible_y_size}"
        ));
    }
    let uv_plane_size = uv_stride
        .checked_mul(height / 2)
        .ok_or_else(|| "NV12 UV plane size overflow".to_string())?;
    let expected = uv_offset
        .checked_add(uv_plane_size)
        .ok_or_else(|| "NV12 buffer size overflow".to_string())?;
    if nv12.len() < expected {
        return Err(format!(
            "NV12 buffer too small: expected at least {expected}, got {}",
            nv12.len()
        ));
    }
    let output_size = width
        .checked_mul(height)
        .and_then(|pixels| pixels.checked_mul(4))
        .ok_or_else(|| "BGRA buffer size overflow".to_string())?;
    bgra.resize(output_size, 0);

    for y in 0..height {
        let y_row = y * y_stride;
        let uv_row = uv_offset + (y / 2) * uv_stride;
        let out_row = y
            .checked_mul(width)
            .and_then(|offset| offset.checked_mul(4))
            .ok_or_else(|| "BGRA row offset overflow".to_string())?;
        for x in 0..width {
            let luma = i32::from(nv12[y_row + x]);
            let uv_index = uv_row + (x & !1);
            let u = i32::from(nv12[uv_index]) - 128;
            let v = i32::from(nv12[uv_index + 1]) - 128;
            let c = (luma - 16).max(0);
            let (red, green, blue) = match color.matrix {
                ColorMatrix::Bt601 => (
                    clamp_u8((298 * c + 409 * v + 128) >> 8),
                    clamp_u8((298 * c - 100 * u - 208 * v + 128) >> 8),
                    clamp_u8((298 * c + 516 * u + 128) >> 8),
                ),
                ColorMatrix::Bt709 => (
                    clamp_u8((298 * c + 459 * v + 128) >> 8),
                    clamp_u8((298 * c - 55 * u - 136 * v + 128) >> 8),
                    clamp_u8((298 * c + 541 * u + 128) >> 8),
                ),
            };
            let output = out_row + x * 4;
            bgra[output] = blue;
            bgra[output + 1] = green;
            bgra[output + 2] = red;
            bgra[output + 3] = 255;
        }
    }
    Ok(())
}

pub fn bgra_size(width: u32, height: u32) -> Result<usize, String> {
    (width as usize)
        .checked_mul(height as usize)
        .and_then(|pixels| pixels.checked_mul(4))
        .ok_or_else(|| "BGRA buffer size overflow".to_string())
}

fn clamp_u8(value: i32) -> u8 {
    value.clamp(0, 255) as u8
}

pub fn run_self_test() -> Result<(), String> {
    let black = [16u8, 16, 16, 16, 128, 128];
    let mut output = Vec::new();
    convert(&black, 2, 2, &mut output)?;
    if output.len() != 16 || output.chunks_exact(4).any(|pixel| pixel[3] != 255) {
        return Err("NV12 to BGRA black-frame conversion failed".to_string());
    }
    let white = [235u8, 235, 235, 235, 128, 128];
    convert(&white, 2, 2, &mut output)?;
    if output.chunks_exact(4).any(|pixel| pixel[0] < 250) {
        return Err("NV12 to BGRA white-frame conversion failed".to_string());
    }

    let padded = [16u8, 16, 99, 99, 16, 16, 99, 99, 128, 128, 77, 77];
    convert_with_strides(&padded, 2, 2, 4, 4, &mut output)?;
    if output.len() != 16 || output.chunks_exact(4).any(|pixel| pixel[0] > 2) {
        return Err("NV12 padded-stride conversion failed".to_string());
    }

    let aligned_height = [16u8, 16, 16, 16, 99, 99, 99, 99, 128, 128];
    convert_with_layout(&aligned_height, 2, 2, 2, 2, 8, &mut output)?;
    if output.chunks_exact(4).any(|pixel| pixel[0] > 2) {
        return Err("NV12 aligned-height UV offset conversion failed".to_string());
    }
    Ok(())
}
