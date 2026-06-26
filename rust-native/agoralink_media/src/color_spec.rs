#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ColorMatrix {
    Bt601,
    Bt709,
}

impl ColorMatrix {
    pub fn parse(value: &str) -> Result<Self, String> {
        match value.to_ascii_lowercase().as_str() {
            "bt601" | "601" => Ok(Self::Bt601),
            "bt709" | "709" | "rec709" => Ok(Self::Bt709),
            _ => Err("color-matrix must be bt601 or bt709".to_string()),
        }
    }

    pub const fn name(self) -> &'static str {
        match self {
            Self::Bt601 => "bt601",
            Self::Bt709 => "bt709",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ColorSpec {
    pub matrix: ColorMatrix,
}

impl ColorSpec {
    pub const REC709_LIMITED_NV12: Self = Self {
        matrix: ColorMatrix::Bt709,
    };

    pub const fn with_matrix(matrix: ColorMatrix) -> Self {
        Self { matrix }
    }

    pub const fn color_primaries(self) -> &'static str {
        match self.matrix {
            ColorMatrix::Bt601 => "bt601",
            ColorMatrix::Bt709 => "rec709",
        }
    }

    pub const fn transfer_characteristics(self) -> &'static str {
        match self.matrix {
            ColorMatrix::Bt601 => "bt601",
            ColorMatrix::Bt709 => "rec709",
        }
    }

    pub const fn yuv_matrix(self) -> &'static str {
        self.matrix.name()
    }

    pub const fn color_range(self) -> &'static str {
        "limited"
    }

    pub const fn pixel_format(self) -> &'static str {
        "NV12"
    }

    pub const fn bit_depth(self) -> u8 {
        8
    }

    pub const fn chroma_subsampling(self) -> &'static str {
        "4:2:0"
    }

    pub fn json_fragment(self) -> String {
        format!(
            r#""color_primaries":"{}","transfer_characteristics":"{}","yuv_matrix":"{}","color_range":"{}","pixel_format":"{}","bit_depth":{},"chroma_subsampling":"{}""#,
            self.color_primaries(),
            self.transfer_characteristics(),
            self.yuv_matrix(),
            self.color_range(),
            self.pixel_format(),
            self.bit_depth(),
            self.chroma_subsampling()
        )
    }
}

impl Default for ColorSpec {
    fn default() -> Self {
        Self::REC709_LIMITED_NV12
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct MediaColorMetadata {
    pub primaries: Option<u32>,
    pub transfer: Option<u32>,
    pub matrix: Option<u32>,
    pub nominal_range: Option<u32>,
    pub default_stride: Option<i32>,
}

impl MediaColorMetadata {
    pub fn resolved_spec(self, fallback: ColorSpec) -> ColorSpec {
        let matrix = match self.matrix {
            Some(2) => ColorMatrix::Bt601,
            Some(1) => ColorMatrix::Bt709,
            _ => fallback.matrix,
        };
        ColorSpec::with_matrix(matrix)
    }

    pub fn json_fragment(self, prefix: &str) -> String {
        format!(
            r#""{prefix}_primaries":{},"{prefix}_transfer":{},"{prefix}_matrix":{},"{prefix}_nominal_range":{},"{prefix}_default_stride":{}"#,
            optional_u32(self.primaries),
            optional_u32(self.transfer),
            optional_u32(self.matrix),
            optional_u32(self.nominal_range),
            optional_i32(self.default_stride)
        )
    }
}

fn optional_u32(value: Option<u32>) -> String {
    value.map_or_else(|| "null".to_string(), |value| value.to_string())
}

fn optional_i32(value: Option<i32>) -> String {
    value.map_or_else(|| "null".to_string(), |value| value.to_string())
}

pub fn run_self_test() -> Result<(), String> {
    let colors = [
        ("black", (0, 0, 0)),
        ("white", (255, 255, 255)),
        ("red", (255, 0, 0)),
        ("green", (0, 255, 0)),
        ("blue", (0, 0, 255)),
        ("gray", (128, 128, 128)),
        ("cyan", (0, 255, 255)),
        ("magenta", (255, 0, 255)),
        ("yellow", (255, 255, 0)),
    ];
    for (name, rgb) in colors {
        let bgra = solid_bgra(4, 4, rgb);
        let restored = roundtrip(&bgra, 4, 4)?;
        assert_pixels_near(name, &bgra, &restored, 5)?;
    }

    for (name, left, right) in [
        ("black-white-edge", (0, 0, 0), (255, 255, 255)),
        ("red-blue-edge", (255, 0, 0), (0, 0, 255)),
        ("green-magenta-edge", (0, 255, 0), (255, 0, 255)),
    ] {
        let bgra = vertical_edge_bgra(8, 4, left, right);
        let restored = roundtrip(&bgra, 8, 4)?;
        assert_pixels_near(name, &bgra, &restored, 8)?;
    }

    let checker = checkerboard_bgra(8, 8);
    let restored = roundtrip(&checker, 8, 8)?;
    let checker_pixels: Vec<&[u8]> = restored.chunks_exact(4).collect();
    if checker_pixels.iter().any(|pixel| pixel[3] != 255)
        || checker_pixels
            .windows(2)
            .all(|pair| pair[0][..3] == pair[1][..3])
    {
        return Err("checkerboard roundtrip produced invalid BGRA".to_string());
    }

    let width = 1920usize;
    let visible_height = 1080usize;
    let allocated_height = 1088usize;
    let uv_offset = width * allocated_height;
    let mut nv12 = vec![128u8; uv_offset + width * visible_height / 2];
    nv12[..width * visible_height].fill(16);
    let mut bgra = Vec::new();
    crate::nv12_to_bgra::convert_with_layout_and_spec(
        &nv12,
        width as u32,
        visible_height as u32,
        width,
        width,
        uv_offset,
        &mut bgra,
        ColorSpec::default(),
    )?;
    if bgra.len() != width * visible_height * 4 || bgra.chunks_exact(4).any(|pixel| pixel[0] > 2) {
        return Err("1080p allocated-height NV12 layout test failed".to_string());
    }
    Ok(())
}

fn roundtrip(bgra: &[u8], width: u32, height: u32) -> Result<Vec<u8>, String> {
    let mut nv12 = vec![0u8; crate::bgra_to_nv12::buffer_size(width, height)?];
    crate::bgra_to_nv12::convert_with_spec(
        bgra,
        width as usize * 4,
        width,
        height,
        &mut nv12,
        ColorSpec::default(),
    )?;
    let mut restored = Vec::new();
    crate::nv12_to_bgra::convert_with_layout_and_spec(
        &nv12,
        width,
        height,
        width as usize,
        width as usize,
        width as usize * height as usize,
        &mut restored,
        ColorSpec::default(),
    )?;
    Ok(restored)
}

fn assert_pixels_near(
    name: &str,
    expected: &[u8],
    actual: &[u8],
    tolerance: u8,
) -> Result<(), String> {
    if expected.len() != actual.len() {
        return Err(format!("{name} roundtrip length mismatch"));
    }
    for (index, (expected, actual)) in expected.iter().zip(actual).enumerate() {
        if expected.abs_diff(*actual) > tolerance {
            return Err(format!(
                "{name} roundtrip mismatch at byte {index}: expected={expected}, actual={actual}, tolerance={tolerance}"
            ));
        }
    }
    Ok(())
}

fn solid_bgra(width: usize, height: usize, rgb: (u8, u8, u8)) -> Vec<u8> {
    let mut output = Vec::with_capacity(width * height * 4);
    for _ in 0..width * height {
        output.extend_from_slice(&[rgb.2, rgb.1, rgb.0, 255]);
    }
    output
}

fn vertical_edge_bgra(
    width: usize,
    height: usize,
    left: (u8, u8, u8),
    right: (u8, u8, u8),
) -> Vec<u8> {
    let mut output = Vec::with_capacity(width * height * 4);
    for _ in 0..height {
        for x in 0..width {
            let rgb = if x < width / 2 { left } else { right };
            output.extend_from_slice(&[rgb.2, rgb.1, rgb.0, 255]);
        }
    }
    output
}

fn checkerboard_bgra(width: usize, height: usize) -> Vec<u8> {
    let mut output = Vec::with_capacity(width * height * 4);
    for y in 0..height {
        for x in 0..width {
            let rgb = if (x + y) % 2 == 0 {
                (255, 0, 0)
            } else {
                (0, 0, 255)
            };
            output.extend_from_slice(&[rgb.2, rgb.1, rgb.0, 255]);
        }
    }
    output
}
