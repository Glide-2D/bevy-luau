use bevy::{
    ecs::component::{ComponentCloneBehavior, ComponentDescriptor, ComponentId, StorageType},
    prelude::*,
};
use lasso::Spur;
use mluau::prelude::*;
use std::{
    alloc::Layout,
    collections::{HashMap, HashSet},
};

use crate::pool::EngineStringPool;
use crate::types::{LuauFieldType, align_up};

#[derive(Debug)]
pub struct DynamicComponentSchema {
    pub name: String,
    pub fields: HashMap<Spur, (usize, LuauFieldType)>,
    pub layout: Layout,
}

#[derive(Resource, Default)]
pub struct SchemaRegistry {
    pub name_to_id: HashMap<String, ComponentId>,
    pub id_to_schema: HashMap<ComponentId, DynamicComponentSchema>,
    pub resource_ids: HashSet<ComponentId>,
    pub resource_data: HashMap<ComponentId, Vec<u8>>,
}

impl SchemaRegistry {
    pub fn build(
        name: String,
        fields: &[(Spur, LuauFieldType)],
    ) -> (DynamicComponentSchema, ComponentDescriptor) {
        let mut offset = 0usize;
        let mut field_offsets = HashMap::new();

        for &(spur, ft) in fields {
            let layout = ft.layout();
            offset = align_up(offset, layout.align());
            field_offsets.insert(spur, (offset, ft));
            offset += layout.size();
        }

        let align = fields
            .iter()
            .map(|(_, t)| t.layout().align())
            .max()
            .unwrap_or(1);
        let size = align_up(offset, align).max(1);
        let layout = Layout::from_size_align(size, align).expect("invalid layout");

        let schema = DynamicComponentSchema {
            name: name.clone(),
            fields: field_offsets,
            layout,
        };

        let descriptor = unsafe {
            ComponentDescriptor::new_with_layout(
                name,
                StorageType::Table,
                layout,
                None,
                true,
                ComponentCloneBehavior::Ignore,
                None,
            )
        };

        (schema, descriptor)
    }

    pub fn insert(&mut self, id: ComponentId, schema: DynamicComponentSchema) {
        self.name_to_id.insert(schema.name.clone(), id);
        self.id_to_schema.insert(id, schema);
    }
}

pub fn extract_resource_table(
    registry: &SchemaRegistry,
    pool: &EngineStringPool,
    lua: &Lua,
    id: ComponentId,
) -> LuaResult<Option<LuaTable>> {
    let Some(data) = registry.resource_data.get(&id) else {
        return Ok(None);
    };
    let Some(schema) = registry.id_to_schema.get(&id) else {
        return Ok(None);
    };

    let table = lua.create_table()?;
    for (&spur, &(offset, ft)) in &schema.fields {
        let lua_str = pool.get_lua_str(spur);
        let field_ptr = unsafe { data.as_ptr().add(offset) };
        match ft {
            LuauFieldType::Bool => table.raw_set(lua_str, unsafe { *field_ptr.cast::<bool>() })?,
            LuauFieldType::Integer => {
                table.raw_set(lua_str, unsafe { *field_ptr.cast::<i64>() })?
            }
            LuauFieldType::Number => table.raw_set(lua_str, unsafe { *field_ptr.cast::<f64>() })?,
            LuauFieldType::Vector4 => {
                let v = unsafe { *field_ptr.cast::<[f32; 4]>() };
                table.raw_set(lua_str, mluau::Vector::new(v[0], v[1], v[2], v[3]))?;
            }
            LuauFieldType::String => {
                let sp = unsafe { *field_ptr.cast::<Spur>() };
                table.raw_set(lua_str, pool.get_lua_str(sp))?;
            }
            LuauFieldType::Buffer(len) => {
                let slice = unsafe { std::slice::from_raw_parts(field_ptr, len) };
                table.raw_set(lua_str, lua.create_buffer(slice)?)?;
            }
        }
    }
    Ok(Some(table))
}

pub fn writeback_resource_table(
    registry: &mut SchemaRegistry,
    pool: &EngineStringPool,
    id: ComponentId,
    table: &LuaTable,
) -> LuaResult<()> {
    let fields: Vec<(Spur, usize, LuauFieldType)> = match registry.id_to_schema.get(&id) {
        Some(s) => s
            .fields
            .iter()
            .map(|(&sp, &(off, ft))| (sp, off, ft))
            .collect(),
        None => return Ok(()),
    };
    let data = match registry.resource_data.get_mut(&id) {
        Some(d) => d,
        None => return Ok(()),
    };
    for (spur, offset, ft) in fields {
        let lua_str = pool.get_lua_str(spur);
        let field_ptr = unsafe { data.as_mut_ptr().add(offset) };
        match (table.raw_get::<LuaValue>(lua_str)?, ft) {
            (LuaValue::Boolean(b), LuauFieldType::Bool) => unsafe {
                std::ptr::write(field_ptr.cast::<bool>(), b)
            },
            (LuaValue::Integer(i), LuauFieldType::Integer) => unsafe {
                std::ptr::write(field_ptr.cast::<i64>(), i)
            },
            (LuaValue::Number(n), LuauFieldType::Number) => unsafe {
                std::ptr::write(field_ptr.cast::<f64>(), n)
            },
            (LuaValue::Vector(v), LuauFieldType::Vector4) => unsafe {
                std::ptr::write(field_ptr.cast::<[f32; 4]>(), [v.x(), v.y(), v.z(), v.w()])
            },
            _ => {}
        }
    }
    Ok(())
}
