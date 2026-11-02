//! Bounds-checked little-endian reads. Every read returns `None` past the end of the slice.

pub fn u8_at(data: &[u8], offset: usize) -> Option<u8> {
    data.get(offset).copied()
}

pub fn u16_at(data: &[u8], offset: usize) -> Option<u16> {
    let bytes = data.get(offset..offset.checked_add(2)?)?;
    Some(u16::from_le_bytes(bytes.try_into().ok()?))
}

pub fn u32_at(data: &[u8], offset: usize) -> Option<u32> {
    let bytes = data.get(offset..offset.checked_add(4)?)?;
    Some(u32::from_le_bytes(bytes.try_into().ok()?))
}

pub fn f32_at(data: &[u8], offset: usize) -> Option<f32> {
    u32_at(data, offset).map(f32::from_bits)
}

pub fn i16_at(data: &[u8], offset: usize) -> Option<i16> {
    u16_at(data, offset).map(|value| i16::from_le_bytes(value.to_le_bytes()))
}

pub fn i32_at(data: &[u8], offset: usize) -> Option<i32> {
    u32_at(data, offset).map(|value| i32::from_le_bytes(value.to_le_bytes()))
}
