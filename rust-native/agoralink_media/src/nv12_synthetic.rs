pub fn buffer_size(width: u32, height: u32) -> Result<usize, String> {
    if width == 0 || height == 0 || width % 2 != 0 || height % 2 != 0 {
        return Err("NV12 width and height must be non-zero even values".to_string());
    }
    let pixels = (width as usize)
        .checked_mul(height as usize)
        .ok_or_else(|| "NV12 dimensions overflow".to_string())?;
    pixels
        .checked_add(pixels / 2)
        .ok_or_else(|| "NV12 buffer size overflow".to_string())
}

pub fn fill_frame(
    buffer: &mut [u8],
    width: u32,
    height: u32,
    frame_index: u64,
) -> Result<(), String> {
    let expected = buffer_size(width, height)?;
    if buffer.len() != expected {
        return Err(format!(
            "NV12 buffer length mismatch: expected {expected}, got {}",
            buffer.len()
        ));
    }

    let width = width as usize;
    let height = height as usize;
    let y_plane_len = width * height;
    let motion = frame_index.wrapping_mul(5) as usize;
    let mut light_row = vec![0u8; width];
    let mut dark_row = vec![0u8; width];
    for column in 0..width {
        let gradient = (column as u8).wrapping_add(motion as u8) >> 1;
        let checker = if ((column + motion) >> 6) & 1 == 0 {
            24
        } else {
            0
        };
        light_row[column] = 56u8.saturating_add(gradient).saturating_add(checker);
        dark_row[column] = 32u8.saturating_add(gradient).saturating_add(checker);
    }
    for row in 0..height {
        let row_start = row * width;
        let use_light = (((row + motion) >> 6) + (frame_index as usize >> 2)) & 1 == 0;
        let source = if use_light { &light_row } else { &dark_row };
        buffer[row_start..row_start + width].copy_from_slice(source);
    }

    let texture_width = (width / 4).max(2);
    let texture_height = (height / 4).max(2);
    let texture_x = (frame_index as usize * 13) % (width - texture_width + 1);
    let texture_y = (frame_index as usize * 7) % (height - texture_height + 1);
    let frame_seed = (frame_index as u32).wrapping_mul(0x045d_9f3b);
    for row in 0..texture_height {
        let row_start = (texture_y + row) * width + texture_x;
        let row_seed = (row as u32).wrapping_mul(0x9e37_79b9) ^ frame_seed;
        for column in 0..texture_width {
            let value = (column as u32)
                .wrapping_mul(73)
                .wrapping_add(row_seed)
                .rotate_left(((row + column) & 7) as u32);
            buffer[row_start + column] = value as u8;
        }
    }

    let uv = &mut buffer[y_plane_len..];
    let chroma_motion = (frame_index % 48) as i16 - 24;
    let u = (112i16 + chroma_motion / 2).clamp(32, 224) as u8;
    let v = (144i16 - chroma_motion / 2).clamp(32, 224) as u8;
    for pair in uv.chunks_exact_mut(2) {
        pair[0] = u;
        pair[1] = v;
    }
    Ok(())
}
