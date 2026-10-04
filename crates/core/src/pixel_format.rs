/// An RFB pixel format (RFC 6143 §7.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PixelFormat {
    pub bits_per_pixel: u8,
    pub depth: u8,
    pub big_endian: bool,
    pub true_colour: bool,
    pub red_max: u16,
    pub green_max: u16,
    pub blue_max: u16,
    pub red_shift: u8,
    pub green_shift: u8,
    pub blue_shift: u8,
}

impl PixelFormat {
    /// 32-bit little-endian with bytes R, G, B, X in memory. This matches an
    /// `Rgba8Unorm` texture, so decoded pixels upload without conversion.
    /// The fourth byte is undefined; renderers must ignore it.
    pub const RGBX8888: PixelFormat = PixelFormat {
        bits_per_pixel: 32,
        depth: 24,
        big_endian: false,
        true_colour: true,
        red_max: 255,
        green_max: 255,
        blue_max: 255,
        red_shift: 0,
        green_shift: 8,
        blue_shift: 16,
    };

    pub const WIRE_LEN: usize = 16;

    pub fn bytes_per_pixel(&self) -> usize {
        usize::from(self.bits_per_pixel).div_ceil(8)
    }

    /// Size of a Tight `TPIXEL`: 3 bytes for 32-bit 8/8/8 true colour,
    /// otherwise a full pixel.
    pub fn tight_pixel_size(&self) -> usize {
        if self.true_colour
            && self.bits_per_pixel == 32
            && self.depth == 24
            && self.red_max == 255
            && self.green_max == 255
            && self.blue_max == 255
        {
            3
        } else {
            self.bytes_per_pixel()
        }
    }

    pub fn from_wire(b: &[u8; Self::WIRE_LEN]) -> PixelFormat {
        PixelFormat {
            bits_per_pixel: b[0],
            depth: b[1],
            big_endian: b[2] != 0,
            true_colour: b[3] != 0,
            red_max: u16::from_be_bytes([b[4], b[5]]),
            green_max: u16::from_be_bytes([b[6], b[7]]),
            blue_max: u16::from_be_bytes([b[8], b[9]]),
            red_shift: b[10],
            green_shift: b[11],
            blue_shift: b[12],
        }
    }

    pub fn to_wire(&self) -> [u8; Self::WIRE_LEN] {
        let mut b = [0u8; Self::WIRE_LEN];
        b[0] = self.bits_per_pixel;
        b[1] = self.depth;
        b[2] = self.big_endian.into();
        b[3] = self.true_colour.into();
        b[4..6].copy_from_slice(&self.red_max.to_be_bytes());
        b[6..8].copy_from_slice(&self.green_max.to_be_bytes());
        b[8..10].copy_from_slice(&self.blue_max.to_be_bytes());
        b[10] = self.red_shift;
        b[11] = self.green_shift;
        b[12] = self.blue_shift;
        b
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_round_trip() {
        let pf = PixelFormat::RGBX8888;
        assert_eq!(PixelFormat::from_wire(&pf.to_wire()), pf);
        assert_eq!(pf.bytes_per_pixel(), 4);
        assert_eq!(pf.tight_pixel_size(), 3);
    }
}
