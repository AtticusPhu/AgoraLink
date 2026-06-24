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
    let output_size = buffer_size(width, height)?;
    if nv12.len() != output_size {
        return Err(format!(
            "NV12 output length mismatch: expected {output_size}, got {}",
            nv12.len()
        ));
    }

    let width = width as usize;
    let height = height as usize;
    let row_bytes = width
        .checked_mul(4)
        .ok_or_else(|| "BGRA row size overflow".to_string())?;
    if row_pitch < row_bytes {
        return Err(format!(
            "BGRA row pitch {row_pitch} is smaller than row bytes {row_bytes}"
        ));
    }
    let required_input = row_pitch
        .checked_mul(height)
        .ok_or_else(|| "BGRA input size overflow".to_string())?;
    if bgra.len() < required_input {
        return Err(format!(
            "BGRA input too short: need {required_input}, got {}",
            bgra.len()
        ));
    }

    let y_plane_len = width * height;
    let source_ptr = bgra.as_ptr();
    let output_ptr = nv12.as_mut_ptr();
    for row in (0..height).step_by(2) {
        let source_top = unsafe { source_ptr.add(row * row_pitch) };
        let source_bottom = unsafe { source_ptr.add((row + 1) * row_pitch) };
        let y_top = unsafe { output_ptr.add(row * width) };
        let y_bottom = unsafe { output_ptr.add((row + 1) * width) };
        let uv_row = unsafe { output_ptr.add(y_plane_len + (row / 2) * width) };
        for column in (0..width).step_by(2) {
            let offset = column * 4;
            unsafe {
                let p00 = source_top.add(offset);
                let p01 = p00.add(4);
                let p10 = source_bottom.add(offset);
                let p11 = p10.add(4);

                let b00 = *p00;
                let g00 = *p00.add(1);
                let r00 = *p00.add(2);
                let b01 = *p01;
                let g01 = *p01.add(1);
                let r01 = *p01.add(2);
                let b10 = *p10;
                let g10 = *p10.add(1);
                let r10 = *p10.add(2);
                let b11 = *p11;
                let g11 = *p11.add(1);
                let r11 = *p11.add(2);

                *y_top.add(column) = rgb_to_y(r00, g00, b00);
                *y_top.add(column + 1) = rgb_to_y(r01, g01, b01);
                *y_bottom.add(column) = rgb_to_y(r10, g10, b10);
                *y_bottom.add(column + 1) = rgb_to_y(r11, g11, b11);

                let red = ((u32::from(r00) + u32::from(r01) + u32::from(r10) + u32::from(r11) + 2)
                    >> 2) as u8;
                let green =
                    ((u32::from(g00) + u32::from(g01) + u32::from(g10) + u32::from(g11) + 2) >> 2)
                        as u8;
                let blue = ((u32::from(b00) + u32::from(b01) + u32::from(b10) + u32::from(b11) + 2)
                    >> 2) as u8;
                let (u, v) = rgb_to_uv(red, green, blue);
                *uv_row.add(column) = u;
                *uv_row.add(column + 1) = v;
            }
        }
    }
    Ok(())
}

#[inline(always)]
fn rgb_to_y(red: u8, green: u8, blue: u8) -> u8 {
    let value = 66i32
        .wrapping_mul(i32::from(red))
        .wrapping_add(129i32.wrapping_mul(i32::from(green)))
        .wrapping_add(25i32.wrapping_mul(i32::from(blue)))
        .wrapping_add(128);
    clamp_u8((value >> 8).wrapping_add(16))
}

#[inline(always)]
fn rgb_to_uv(red: u8, green: u8, blue: u8) -> (u8, u8) {
    let red = i32::from(red);
    let green = i32::from(green);
    let blue = i32::from(blue);
    let u = (-38i32)
        .wrapping_mul(red)
        .wrapping_sub(74i32.wrapping_mul(green))
        .wrapping_add(112i32.wrapping_mul(blue))
        .wrapping_add(128);
    let v = 112i32
        .wrapping_mul(red)
        .wrapping_sub(94i32.wrapping_mul(green))
        .wrapping_sub(18i32.wrapping_mul(blue))
        .wrapping_add(128);
    let u = (u >> 8).wrapping_add(128);
    let v = (v >> 8).wrapping_add(128);
    (clamp_u8(u), clamp_u8(v))
}

#[inline(always)]
fn clamp_u8(value: i32) -> u8 {
    value.clamp(0, 255) as u8
}

pub fn run_self_test() -> Result<(), String> {
    if buffer_size(2, 2)? != 6 {
        return Err("2x2 NV12 buffer size must be 6".to_string());
    }
    if buffer_size(3, 2).is_ok() || buffer_size(2, 3).is_ok() {
        return Err("odd BGRA dimensions were accepted".to_string());
    }

    let black = [0u8, 0, 0, 255, 0, 0, 0, 255, 0, 0, 0, 255, 0, 0, 0, 255];
    let mut black_nv12 = vec![0u8; 6];
    convert(&black, 8, 2, 2, &mut black_nv12)?;
    if black_nv12 != [16, 16, 16, 16, 128, 128] {
        return Err(format!("unexpected black NV12 values: {black_nv12:?}"));
    }

    let white = [
        255u8, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255,
    ];
    let mut white_nv12 = vec![0u8; 6];
    convert(&white, 8, 2, 2, &mut white_nv12)?;
    if white_nv12 != [235, 235, 235, 235, 128, 128] {
        return Err(format!("unexpected white NV12 values: {white_nv12:?}"));
    }

    let mut padded = vec![0xCCu8; 24];
    padded[0..8].copy_from_slice(&black[0..8]);
    padded[12..20].copy_from_slice(&black[8..16]);
    let mut padded_nv12 = vec![0u8; 6];
    convert(&padded, 12, 2, 2, &mut padded_nv12)?;
    if padded_nv12 != black_nv12 {
        return Err("row-pitch padding changed BGRA conversion output".to_string());
    }
    Ok(())
}
