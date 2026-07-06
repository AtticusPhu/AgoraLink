#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VideoDimensions {
    pub width: u32,
    pub height: u32,
}

#[derive(Clone, Copy)]
struct NalUnit {
    start: usize,
    payload_start: usize,
    end: usize,
    nal_type: u8,
}

#[derive(Clone, Debug, Default)]
pub struct AnnexBParameterSets {
    sps: Option<Vec<u8>>,
    pps: Option<Vec<u8>>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct AnnexBNalSummary {
    pub has_sps: bool,
    pub has_pps: bool,
    pub has_idr_slice: bool,
    pub has_non_idr_slice: bool,
    pub has_aud: bool,
}

impl AnnexBParameterSets {
    pub fn update_from(&mut self, data: &[u8]) {
        for nal in scan_nals(data) {
            match nal.nal_type {
                7 => self.sps = Some(data[nal.start..nal.end].to_vec()),
                8 => self.pps = Some(data[nal.start..nal.end].to_vec()),
                _ => {}
            }
        }
    }

    pub fn prepend_missing_to_keyframe(&self, data: &[u8]) -> Result<Vec<u8>, String> {
        let (has_sps, has_pps) = parameter_set_presence(data);
        if has_sps && has_pps {
            return Ok(data.to_vec());
        }

        let mut output = Vec::with_capacity(
            data.len()
                + self.sps.as_ref().map_or(0, Vec::len)
                + self.pps.as_ref().map_or(0, Vec::len),
        );
        if !has_sps {
            output.extend_from_slice(
                self.sps
                    .as_deref()
                    .ok_or_else(|| "keyframe is missing cached SPS".to_string())?,
            );
        }
        if !has_pps {
            output.extend_from_slice(
                self.pps
                    .as_deref()
                    .ok_or_else(|| "keyframe is missing cached PPS".to_string())?,
            );
        }
        output.extend_from_slice(data);
        Ok(output)
    }
}

pub fn nal_types(data: &[u8]) -> Vec<u8> {
    scan_nals(data)
        .into_iter()
        .map(|nal| nal.nal_type)
        .collect()
}

pub fn summarize_nals(data: &[u8]) -> AnnexBNalSummary {
    let mut summary = AnnexBNalSummary::default();
    for nal in scan_nals(data) {
        match nal.nal_type {
            1 => summary.has_non_idr_slice = true,
            5 => summary.has_idr_slice = true,
            7 => summary.has_sps = true,
            8 => summary.has_pps = true,
            9 => summary.has_aud = true,
            _ => {}
        }
    }
    summary
}

pub fn parameter_set_presence(data: &[u8]) -> (bool, bool) {
    let summary = summarize_nals(data);
    (summary.has_sps, summary.has_pps)
}

pub fn split_access_units(data: &[u8]) -> Result<Vec<Vec<u8>>, String> {
    let nals = scan_nals(data);
    if nals.is_empty() {
        return Err("input does not contain Annex-B start codes".to_string());
    }

    let has_aud = nals.iter().any(|nal| nal.nal_type == 9);
    let mut units = Vec::new();
    let mut unit_start = nals[0].start;
    let mut has_vcl = false;

    for nal in &nals {
        let is_vcl = (1..=5).contains(&nal.nal_type);
        let starts_new = if has_aud {
            nal.nal_type == 9 && has_vcl
        } else {
            is_vcl && has_vcl && first_mb_in_slice(data, *nal) == Some(0)
        };
        if starts_new {
            if nal.start > unit_start {
                units.push(data[unit_start..nal.start].to_vec());
            }
            unit_start = nal.start;
            has_vcl = false;
        }
        has_vcl |= is_vcl;
    }

    if has_vcl && unit_start < data.len() {
        units.push(data[unit_start..].to_vec());
    }
    if units.is_empty() {
        return Err("Annex-B stream contains no video access units".to_string());
    }
    Ok(units)
}

pub fn dimensions_from_sps(data: &[u8]) -> Result<VideoDimensions, String> {
    let sps = scan_nals(data)
        .into_iter()
        .find(|nal| nal.nal_type == 7)
        .ok_or_else(|| "Annex-B stream does not contain an SPS".to_string())?;
    parse_sps_dimensions(&data[sps.payload_start..sps.end])
}

fn scan_nals(data: &[u8]) -> Vec<NalUnit> {
    let mut starts = Vec::new();
    let mut index = 0usize;
    while index + 3 < data.len() {
        let prefix_len = if data[index..].starts_with(&[0, 0, 0, 1]) {
            4
        } else if data[index..].starts_with(&[0, 0, 1]) {
            3
        } else {
            index += 1;
            continue;
        };
        let payload_start = index + prefix_len;
        if payload_start < data.len() {
            starts.push((index, payload_start));
        }
        index = payload_start;
    }

    starts
        .iter()
        .enumerate()
        .filter_map(|(position, (start, payload_start))| {
            let end = starts
                .get(position + 1)
                .map_or(data.len(), |(next, _)| *next);
            data.get(*payload_start).map(|header| NalUnit {
                start: *start,
                payload_start: *payload_start,
                end,
                nal_type: header & 0x1f,
            })
        })
        .collect()
}

fn first_mb_in_slice(data: &[u8], nal: NalUnit) -> Option<u32> {
    let payload = data.get(nal.payload_start + 1..nal.end)?;
    let rbsp = remove_emulation_prevention(payload);
    BitReader::new(&rbsp).read_ue().ok()
}

fn parse_sps_dimensions(nal: &[u8]) -> Result<VideoDimensions, String> {
    if nal.is_empty() || nal[0] & 0x1f != 7 {
        return Err("invalid SPS NAL unit".to_string());
    }
    let rbsp = remove_emulation_prevention(&nal[1..]);
    let mut bits = BitReader::new(&rbsp);
    let profile_idc = bits.read_bits(8)? as u8;
    bits.skip_bits(8)?;
    bits.skip_bits(8)?;
    bits.read_ue()?;

    let mut chroma_format_idc = 1u32;
    let mut separate_colour_plane_flag = false;
    if matches!(
        profile_idc,
        44 | 83 | 86 | 100 | 110 | 118 | 122 | 128 | 134 | 135 | 138 | 139 | 244
    ) {
        chroma_format_idc = bits.read_ue()?;
        if chroma_format_idc > 3 {
            return Err(format!(
                "unsupported SPS chroma_format_idc: {chroma_format_idc}"
            ));
        }
        if chroma_format_idc == 3 {
            separate_colour_plane_flag = bits.read_bit()? != 0;
        }
        bits.read_ue()?;
        bits.read_ue()?;
        bits.skip_bits(1)?;
        if bits.read_bit()? != 0 {
            let scaling_count = if chroma_format_idc == 3 { 12 } else { 8 };
            for index in 0..scaling_count {
                if bits.read_bit()? != 0 {
                    skip_scaling_list(&mut bits, if index < 6 { 16 } else { 64 })?;
                }
            }
        }
    }

    bits.read_ue()?;
    let pic_order_cnt_type = bits.read_ue()?;
    match pic_order_cnt_type {
        0 => {
            bits.read_ue()?;
        }
        1 => {
            bits.skip_bits(1)?;
            bits.read_se()?;
            bits.read_se()?;
            let cycle = bits.read_ue()?;
            for _ in 0..cycle {
                bits.read_se()?;
            }
        }
        _ => {}
    }
    bits.read_ue()?;
    bits.skip_bits(1)?;
    let pic_width_in_mbs_minus1 = bits.read_ue()?;
    let pic_height_in_map_units_minus1 = bits.read_ue()?;
    let frame_mbs_only_flag = bits.read_bit()? != 0;
    if !frame_mbs_only_flag {
        bits.skip_bits(1)?;
    }
    bits.skip_bits(1)?;

    let mut crop_left = 0u32;
    let mut crop_right = 0u32;
    let mut crop_top = 0u32;
    let mut crop_bottom = 0u32;
    if bits.read_bit()? != 0 {
        crop_left = bits.read_ue()?;
        crop_right = bits.read_ue()?;
        crop_top = bits.read_ue()?;
        crop_bottom = bits.read_ue()?;
    }

    let frame_factor = if frame_mbs_only_flag { 1 } else { 2 };
    let coded_width = (pic_width_in_mbs_minus1 + 1)
        .checked_mul(16)
        .ok_or_else(|| "SPS width overflow".to_string())?;
    let coded_height = (pic_height_in_map_units_minus1 + 1)
        .checked_mul(16 * frame_factor)
        .ok_or_else(|| "SPS height overflow".to_string())?;

    let effective_chroma = if separate_colour_plane_flag {
        0
    } else {
        chroma_format_idc
    };
    let crop_unit_x = match effective_chroma {
        0 | 3 => 1,
        1 | 2 => 2,
        _ => unreachable!(),
    };
    let crop_unit_y = match effective_chroma {
        0 => frame_factor,
        1 => 2 * frame_factor,
        2 | 3 => frame_factor,
        _ => unreachable!(),
    };
    let crop_width = (crop_left + crop_right)
        .checked_mul(crop_unit_x)
        .ok_or_else(|| "SPS horizontal crop overflow".to_string())?;
    let crop_height = (crop_top + crop_bottom)
        .checked_mul(crop_unit_y)
        .ok_or_else(|| "SPS vertical crop overflow".to_string())?;
    let width = coded_width
        .checked_sub(crop_width)
        .ok_or_else(|| "SPS horizontal crop exceeds coded width".to_string())?;
    let height = coded_height
        .checked_sub(crop_height)
        .ok_or_else(|| "SPS vertical crop exceeds coded height".to_string())?;
    if width == 0 || height == 0 {
        return Err("SPS produced zero dimensions".to_string());
    }
    Ok(VideoDimensions { width, height })
}

fn remove_emulation_prevention(data: &[u8]) -> Vec<u8> {
    let mut rbsp = Vec::with_capacity(data.len());
    let mut zeros = 0u8;
    for &byte in data {
        if zeros >= 2 && byte == 3 {
            zeros = 0;
            continue;
        }
        rbsp.push(byte);
        zeros = if byte == 0 {
            zeros.saturating_add(1)
        } else {
            0
        };
    }
    rbsp
}

fn skip_scaling_list(bits: &mut BitReader<'_>, size: usize) -> Result<(), String> {
    let mut last_scale = 8i32;
    let mut next_scale = 8i32;
    for _ in 0..size {
        if next_scale != 0 {
            let delta = bits.read_se()?;
            next_scale = (last_scale + delta + 256) % 256;
        }
        if next_scale != 0 {
            last_scale = next_scale;
        }
    }
    Ok(())
}

struct BitReader<'a> {
    data: &'a [u8],
    bit_offset: usize,
}

impl<'a> BitReader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self {
            data,
            bit_offset: 0,
        }
    }

    fn read_bit(&mut self) -> Result<u32, String> {
        if self.bit_offset >= self.data.len() * 8 {
            return Err("unexpected end of H.264 bitstream".to_string());
        }
        let byte = self.data[self.bit_offset / 8];
        let shift = 7 - self.bit_offset % 8;
        self.bit_offset += 1;
        Ok(u32::from((byte >> shift) & 1))
    }

    fn read_bits(&mut self, count: usize) -> Result<u32, String> {
        if count > 32 {
            return Err("bit read exceeds u32".to_string());
        }
        let mut value = 0u32;
        for _ in 0..count {
            value = (value << 1) | self.read_bit()?;
        }
        Ok(value)
    }

    fn skip_bits(&mut self, count: usize) -> Result<(), String> {
        for _ in 0..count {
            self.read_bit()?;
        }
        Ok(())
    }

    fn read_ue(&mut self) -> Result<u32, String> {
        let mut leading_zero_bits = 0usize;
        while self.read_bit()? == 0 {
            leading_zero_bits += 1;
            if leading_zero_bits > 31 {
                return Err("Exp-Golomb value exceeds u32".to_string());
            }
        }
        let suffix = self.read_bits(leading_zero_bits)?;
        Ok(((1u32 << leading_zero_bits) - 1) + suffix)
    }

    fn read_se(&mut self) -> Result<i32, String> {
        let code_num = self.read_ue()?;
        if code_num % 2 == 0 {
            Ok(-((code_num / 2) as i32))
        } else {
            Ok(code_num.div_ceil(2) as i32)
        }
    }
}

pub fn run_self_test() -> Result<(), String> {
    let stream = [
        &[0, 0, 0, 1, 9, 0xf0][..],
        &[0, 0, 1, 7, 1][..],
        &[0, 0, 1, 8, 2][..],
        &[0, 0, 1, 5, 0x80][..],
        &[0, 0, 0, 1, 9, 0xf0][..],
        &[0, 0, 1, 1, 0x80][..],
    ]
    .concat();
    let units = split_access_units(&stream)?;
    if units.len() != 2 {
        return Err(format!(
            "Annex-B access unit split expected 2, got {}",
            units.len()
        ));
    }
    let sps = [
        0x67, 0x4d, 0x40, 0x28, 0x96, 0x56, 0x03, 0xc0, 0x11, 0x3f, 0x2e, 0x02, 0x20, 0x00, 0x00,
        0x03, 0x00, 0x20, 0x00, 0x00, 0x07, 0x81, 0xb4, 0x11, 0x08, 0xa7,
    ];
    let dimensions = parse_sps_dimensions(&sps)?;
    if dimensions
        != (VideoDimensions {
            width: 1920,
            height: 1080,
        })
    {
        return Err(format!("SPS dimension parse mismatch: {dimensions:?}"));
    }

    let config = [&[0, 0, 0, 1, 7, 0x64][..], &[0, 0, 0, 1, 8, 0xee][..]].concat();
    let keyframe = [0, 0, 0, 1, 5, 0x80];
    let mut parameter_sets = AnnexBParameterSets::default();
    parameter_sets.update_from(&config);
    let repaired = parameter_sets.prepend_missing_to_keyframe(&keyframe)?;
    let repaired_summary = summarize_nals(&repaired);
    if !repaired_summary.has_sps
        || !repaired_summary.has_pps
        || !repaired_summary.has_idr_slice
        || repaired_summary.has_non_idr_slice
    {
        return Err("Annex-B keyframe parameter-set repetition failed".to_string());
    }
    Ok(())
}
