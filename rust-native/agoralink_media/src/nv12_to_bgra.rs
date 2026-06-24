pub fn convert(nv12: &[u8], width: u32, height: u32, bgra: &mut Vec<u8>) -> Result<(), String> {
    if width == 0 || height == 0 || width % 2 != 0 || height % 2 != 0 {
        return Err("NV12 width and height must be non-zero even values".to_string());
    }
    let width = width as usize;
    let height = height as usize;
    let y_size = width
        .checked_mul(height)
        .ok_or_else(|| "NV12 Y plane size overflow".to_string())?;
    let expected = y_size
        .checked_add(y_size / 2)
        .ok_or_else(|| "NV12 buffer size overflow".to_string())?;
    if nv12.len() < expected {
        return Err(format!(
            "NV12 buffer too small: expected at least {expected}, got {}",
            nv12.len()
        ));
    }
    let output_size = y_size
        .checked_mul(4)
        .ok_or_else(|| "BGRA buffer size overflow".to_string())?;
    bgra.resize(output_size, 0);

    for y in 0..height {
        let y_row = y * width;
        let uv_row = y_size + (y / 2) * width;
        let out_row = y_row * 4;
        for x in 0..width {
            let luma = i32::from(nv12[y_row + x]);
            let uv_index = uv_row + (x & !1);
            let u = i32::from(nv12[uv_index]) - 128;
            let v = i32::from(nv12[uv_index + 1]) - 128;
            let c = (luma - 16).max(0);
            let red = clamp_u8((298 * c + 409 * v + 128) >> 8);
            let green = clamp_u8((298 * c - 100 * u - 208 * v + 128) >> 8);
            let blue = clamp_u8((298 * c + 516 * u + 128) >> 8);
            let output = out_row + x * 4;
            bgra[output] = blue;
            bgra[output + 1] = green;
            bgra[output + 2] = red;
            bgra[output + 3] = 255;
        }
    }
    Ok(())
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
    Ok(())
}
