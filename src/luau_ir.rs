#![expect(unused)]

use bevy::prelude::*;
use lasso::{Rodeo, Spur};
use mluau::prelude::*;
use smallvec::SmallVec;
use std::collections::{HashMap, hash_map::Entry};

pub struct EngineStringPool {
    pub rodeo: Rodeo,
    pub bridge: HashMap<Spur, LuaString>,
}

impl EngineStringPool {
    pub fn register_string(&mut self, lua: &Lua, text: &str) -> LuaResult<Spur> {
        let spur = self.rodeo.get_or_intern(text);

        match self.bridge.entry(spur) {
            Entry::Occupied(_) => {}
            Entry::Vacant(entry) => {
                let lua_string = lua.create_string(text)?;
                entry.insert(lua_string);
            }
        }

        Ok(spur)
    }

    pub fn spur_from_lua_str(&self, s: &LuaString) -> Option<Spur> {
        let borrowed = s.to_str().ok()?;
        self.rodeo.get(&*borrowed)
    }

    #[inline]
    pub fn lua_str(&self, spur: Spur) -> &LuaString {
        self.bridge.get(&spur).expect("unregistered spur")
    }
}

pub enum LuauFrameIr {
    Number(f64),
    String(Spur),
}

#[derive(Clone, Copy, Debug)]
pub enum LuauFieldType {
    Bool,          // bool
    Integer,       // i64
    Number,        // f64
    LuauInt,       // i64 (genuinly have no idea what the difference is)
    Vector3,       // [f32; 3]
    Vector4,       // [f32; 4] (luau-vector4 feature)
    String,        // Spur = u32
    Buffer(usize), // fixed-size [u8; N]
}

impl LuauFieldType {
    pub fn layout(self) -> std::alloc::Layout {
        use std::alloc::Layout;
        match self {
            Self::Bool => Layout::new::<bool>(),
            Self::Integer => Layout::new::<i64>(),
            Self::Number => Layout::new::<f64>(),
            Self::LuauInt => Layout::new::<i64>(),
            Self::Vector3 => Layout::new::<[f32; 3]>(),
            Self::Vector4 => Layout::new::<[f32; 4]>(),
            Self::String => Layout::new::<Spur>(), // Spur is u32 (note its nonzero<u32>)
            Self::Buffer(n) => Layout::array::<u8>(n).unwrap(), // who the fuck would make a luau buffer with 9,223,372,036,854,775,807 bytes 😭
        }
    }
}

pub struct LuauFrameIrLayout {
    pub fields: SmallVec<[(Spur, LuauFrameIr); 8]>,
}

impl LuauFrameIrLayout {
    pub fn write_to_table(&self, table: &LuaTable, pool: &EngineStringPool) -> LuaResult<()> {
        for (key_spur, val) in &self.fields {
            let lua_key = pool.lua_str(*key_spur).clone();
            match val {
                LuauFrameIr::Number(n) => table.raw_set(lua_key, *n)?,
                LuauFrameIr::String(s) => table.raw_set(lua_key, pool.lua_str(*s).clone())?,
            }
        }
        Ok(())
    }

    pub fn read_from_table(
        table: &LuaTable,
        schema: &[Spur],
        pool: &EngineStringPool,
    ) -> LuaResult<Self> {
        let mut fields = SmallVec::new();
        for &key_spur in schema {
            match table.raw_get::<LuaValue>(pool.lua_str(key_spur).clone())? {
                LuaValue::Number(n) => fields.push((key_spur, LuauFrameIr::Number(n))),
                LuaValue::Integer(i) => fields.push((key_spur, LuauFrameIr::Number(i as f64))),
                LuaValue::String(s) => {
                    if let Some(spur) = pool.spur_from_lua_str(&s) {
                        fields.push((key_spur, LuauFrameIr::String(spur)));
                    }
                    // strings should prob call pool.register_string here ngl
                }
                LuaValue::Nil => {}
                _ => {}
            }
        }
        Ok(Self { fields })
    }
}

pub struct LuauScriptContext {
    snapshot_key: LuaRegistryKey,
    func_key: LuaRegistryKey,
    pub output_schema: Vec<Spur>,
}

impl LuauScriptContext {
    pub fn new(lua: &Lua, source: &str) -> LuaResult<Self> {
        let func: LuaFunction = lua.load(source).into_function()?;
        let table = lua.create_table()?;
        Ok(Self {
            snapshot_key: lua.create_registry_value(table)?,
            func_key: lua.create_registry_value(func)?,
            output_schema: Vec::new(),
        })
    }

    pub fn call(
        &self,
        lua: &Lua,
        input: &LuauFrameIrLayout,
        pool: &EngineStringPool,
    ) -> LuaResult<LuauFrameIrLayout> {
        let snapshot: LuaTable = lua.registry_value(&self.snapshot_key)?;
        input.write_to_table(&snapshot, pool)?;

        let func: LuaFunction = lua.registry_value(&self.func_key)?;
        let result: LuaTable = func.call(snapshot)?;

        LuauFrameIrLayout::read_from_table(&result, &self.output_schema, pool)
    }
}
