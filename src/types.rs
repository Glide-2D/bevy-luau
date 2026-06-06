use lasso::Spur;
use std::alloc::Layout;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LuauFieldType {
    Bool,
    Integer,
    Number,
    Vector4,
    String,
    Buffer(usize),
}

impl LuauFieldType {
    pub fn layout(self) -> Layout {
        match self {
            Self::Bool => Layout::new::<bool>(),
            Self::Integer => Layout::new::<i64>(),
            Self::Number => Layout::new::<f64>(),
            Self::Vector4 => Layout::new::<[f32; 4]>(),
            Self::String => Layout::new::<Spur>(),
            Self::Buffer(n) => Layout::array::<u8>(n).unwrap(),
        }
    }
}

#[derive(Clone, Copy)]
pub enum LuaSchedule {
    Startup,
    Update,
}

pub(crate) fn align_up(offset: usize, align: usize) -> usize {
    (offset + align - 1) & !(align - 1)
}
