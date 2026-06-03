#![expect(unused)]

use bevy::ecs::component::{ComponentCloneBehavior, ComponentDescriptor, ComponentId, StorageType};
use bevy::prelude::*;
use bevy::ptr::OwningPtr;
use lasso::{Rodeo, Spur};
use mluau::prelude::*;
use smallvec::SmallVec;
use std::ptr::NonNull;
use std::{
    alloc::Layout,
    collections::{HashMap, hash_map::Entry},
};

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
    Bool(bool),
    Integer(i64),
    Number(f64),
    Vector3([f32; 3]),
    Vector4([f32; 4]),
    String(Spur),
    Buffer(Vec<u8>),
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

#[derive(Debug)]
pub struct DynamicComponentSchema {
    pub name: String,
    pub fields: HashMap<Spur, (usize, LuauFieldType)>, // offset, field
    pub layout: Layout,
}

impl DynamicComponentSchema {
    pub fn build(name: String, fields: &[(Spur, LuauFieldType)]) -> Self {
        let mut struct_layout = Layout::from_size_align(0, 1).unwrap();
        let mut field_offsets = HashMap::with_capacity(fields.len());

        let mut fields = fields.to_vec();
        fields.sort_by_key(|(spur, _)| *spur);

        for (spur, field_type) in fields {
            let field_layout = field_type.layout();

            let (new_layout, offset) = struct_layout.extend(field_layout).unwrap();
            struct_layout = new_layout;

            field_offsets.insert(spur, (offset, field_type));
        }

        let layout = struct_layout.pad_to_align();

        Self {
            name,
            fields: field_offsets,
            layout,
        }
    }
}

pub fn register_dynamic_component(
    world: &mut World,
    schema: &DynamicComponentSchema,
) -> ComponentId {
    let descriptor = unsafe {
        ComponentDescriptor::new_with_layout(
            schema.name.clone(),
            StorageType::Table,
            schema.layout,
            None,
            true,
            ComponentCloneBehavior::Ignore,
            None,
        )
    };

    world.register_component_with_descriptor(descriptor)
}

pub unsafe fn insert_luau_data(
    world: &mut World,
    entity: Entity,
    component_id: ComponentId,
    schema: &DynamicComponentSchema,
    data: &LuauFrameIrLayout,
) {
    unsafe {
        let scratch_ptr = std::alloc::alloc_zeroed(schema.layout);
        if scratch_ptr.is_null() {
            std::alloc::handle_alloc_error(schema.layout);
        }

        for (spur, val) in &data.fields {
            if let Some(&(offset, field_type)) = schema.fields.get(spur) {
                let field_ptr = scratch_ptr.add(offset);

                match val {
                    LuauFrameIr::Bool(b) => {
                        if matches!(field_type, LuauFieldType::Bool) {
                            std::ptr::write(field_ptr as *mut bool, *b);
                        }
                    }
                    LuauFrameIr::Integer(i) => {
                        if matches!(field_type, LuauFieldType::Integer | LuauFieldType::LuauInt) {
                            std::ptr::write(field_ptr as *mut i64, *i);
                        }
                    }
                    LuauFrameIr::Number(n) => {
                        if matches!(field_type, LuauFieldType::Number) {
                            std::ptr::write(field_ptr as *mut f64, *n);
                        }
                    }
                    LuauFrameIr::Vector3(v) => {
                        if matches!(field_type, LuauFieldType::Vector3) {
                            std::ptr::write(field_ptr as *mut [f32; 3], *v);
                        }
                    }
                    LuauFrameIr::Vector4(v) => {
                        if matches!(field_type, LuauFieldType::Vector4) {
                            std::ptr::write(field_ptr as *mut [f32; 4], *v);
                        }
                    }
                    LuauFrameIr::String(s) => {
                        if matches!(field_type, LuauFieldType::String) {
                            std::ptr::write(field_ptr as *mut Spur, *s);
                        }
                    }
                    LuauFrameIr::Buffer(buf) => {
                        if let LuauFieldType::Buffer(len) = field_type {
                            let copy_len = buf.len().min(len);
                            std::ptr::copy_nonoverlapping(buf.as_ptr(), field_ptr, copy_len);
                        }
                    }
                }
            }
        }

        let non_null = NonNull::new(scratch_ptr).unwrap();
        let owning_ptr = OwningPtr::new(non_null);

        world
            .entity_mut(entity)
            .insert_by_id(component_id, owning_ptr);
    }
}

pub struct LuauFrameIrLayout {
    pub fields: SmallVec<[(Spur, LuauFrameIr); 8]>,
}

impl LuauFrameIrLayout {
    // Note: write_to_table needs the `&Lua` context to instantiate buffers and vectors
    pub fn write_to_table(
        &self,
        lua: &Lua,
        table: &LuaTable,
        pool: &EngineStringPool,
    ) -> LuaResult<()> {
        for (key_spur, val) in &self.fields {
            let lua_key = pool.lua_str(*key_spur).clone();
            match val {
                LuauFrameIr::Bool(b) => table.raw_set(lua_key, *b)?,
                LuauFrameIr::Integer(i) => table.raw_set(lua_key, *i)?,
                LuauFrameIr::Number(n) => table.raw_set(lua_key, *n)?,
                LuauFrameIr::String(s) => table.raw_set(lua_key, pool.lua_str(*s).clone())?,

                LuauFrameIr::Vector3([x, y, z]) => {
                    table.raw_set(lua_key, LuaValue::Vector(LuaVector::new(*x, *y, *z)))?
                }

                LuauFrameIr::Buffer(buf) => {
                    let lua_buf = lua.create_buffer(buf)?;
                    table.raw_set(lua_key, lua_buf)?;
                }
                _ => {} // Handle any cfg feature mismatches
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
                LuaValue::Boolean(b) => fields.push((key_spur, LuauFrameIr::Bool(b))),
                LuaValue::Integer(i) => fields.push((key_spur, LuauFrameIr::Integer(i))),
                LuaValue::Number(n) => fields.push((key_spur, LuauFrameIr::Number(n))),
                LuaValue::String(s) => {
                    if let Some(spur) = pool.spur_from_lua_str(&s) {
                        fields.push((key_spur, LuauFrameIr::String(spur)));
                    }
                    // strings should prob call pool.register_string here ngl
                }

                LuaValue::Vector(vector) => {
                    fields.push((
                        key_spur,
                        LuauFrameIr::Vector3([vector.x(), vector.y(), vector.z()]),
                    ));
                }

                LuaValue::Buffer(b) => {
                    let bytes = b.to_vec();
                    fields.push((key_spur, LuauFrameIr::Buffer(bytes)));
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
        input.write_to_table(lua, &snapshot, pool)?;

        let func: LuaFunction = lua.registry_value(&self.func_key)?;
        let result: LuaTable = func.call(snapshot)?;

        LuauFrameIrLayout::read_from_table(&result, &self.output_schema, pool)
    }
}
