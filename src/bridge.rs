use bevy::{ecs::component::ComponentId, prelude::*, ptr::OwningPtr};
use lasso::Spur;
use mluau::prelude::*;
use std::{
    alloc::{alloc_zeroed, dealloc},
    ptr::NonNull,
};

use crate::pool::EngineStringPool;
use crate::schema::SchemaRegistry;
use crate::types::LuauFieldType;

pub struct DynamicComponentBridge;

impl DynamicComponentBridge {
    /// # Safety
    pub unsafe fn insert_from_table(
        world: &mut World,
        entity: Entity,
        component_id: ComponentId,
        registry: &SchemaRegistry,
        pool: &mut EngineStringPool,
        table: &LuaTable,
        lua: &Lua,
    ) -> LuaResult<()> {
        let schema = registry
            .id_to_schema
            .get(&component_id)
            .expect("schema not registered");
        let layout = schema.layout;
        let scratch = unsafe { alloc_zeroed(layout) };
        if scratch.is_null() {
            std::alloc::handle_alloc_error(layout);
        }

        for (&spur, &(offset, ft)) in &schema.fields {
            let lua_str = pool.get_lua_str(spur);
            let field_ptr = unsafe { scratch.add(offset) };
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
                (LuaValue::String(s), LuauFieldType::String) => {
                    let sp = pool.intern(lua, s.to_str()?.as_ref());
                    unsafe { std::ptr::write(field_ptr.cast::<Spur>(), sp) };
                }
                (LuaValue::Buffer(b), LuauFieldType::Buffer(len)) => unsafe {
                    std::ptr::copy_nonoverlapping(
                        b.to_pointer().cast::<u8>(),
                        field_ptr,
                        b.len().min(len),
                    );
                },
                _ => {}
            }
        }

        let owning = unsafe { OwningPtr::new(NonNull::new_unchecked(scratch)) };
        unsafe { world.entity_mut(entity).insert_by_id(component_id, owning) };
        unsafe { dealloc(scratch, layout) };
        Ok(())
    }

    /// # Safety
    pub unsafe fn insert_default(
        world: &mut World,
        entity: Entity,
        component_id: ComponentId,
        registry: &SchemaRegistry,
    ) {
        let schema = registry
            .id_to_schema
            .get(&component_id)
            .expect("schema not registered");
        let layout = schema.layout;
        let scratch = unsafe { alloc_zeroed(layout) };
        if scratch.is_null() {
            std::alloc::handle_alloc_error(layout);
        }
        let owning = unsafe { OwningPtr::new(NonNull::new_unchecked(scratch)) };
        unsafe { world.entity_mut(entity).insert_by_id(component_id, owning) };
        unsafe { dealloc(scratch, layout) };
    }

    /// # Safety
    pub unsafe fn extract_to_table(
        world: &World,
        entity: Entity,
        component_id: ComponentId,
        registry: &SchemaRegistry,
        pool: &EngineStringPool,
        lua: &Lua,
    ) -> LuaResult<Option<LuaTable>> {
        let Some(schema) = registry.id_to_schema.get(&component_id) else {
            return Ok(None);
        };
        let Ok(ptr) = world.entity(entity).get_by_id(component_id) else {
            return Ok(None);
        };

        let raw = ptr.as_ptr();
        let table = lua.create_table()?;

        for (&spur, &(offset, ft)) in &schema.fields {
            let lua_str = pool.get_lua_str(spur);
            let field_ptr = unsafe { raw.add(offset) };
            match ft {
                LuauFieldType::Bool => {
                    table.raw_set(lua_str, unsafe { *field_ptr.cast::<bool>() })?
                }
                LuauFieldType::Integer => {
                    table.raw_set(lua_str, unsafe { *field_ptr.cast::<i64>() })?
                }
                LuauFieldType::Number => {
                    table.raw_set(lua_str, unsafe { *field_ptr.cast::<f64>() })?
                }
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pool::EngineStringPool;
    use crate::schema::SchemaRegistry;
    use crate::types::LuauFieldType;
    use std::f64;

    #[test]
    fn roundtrip() {
        let lua = Lua::new();
        let mut world = World::new();
        let mut pool = EngineStringPool::default();
        let mut registry = SchemaRegistry::default();

        let fields = vec![
            (pool.intern(&lua, "a"), LuauFieldType::Integer),
            (pool.intern(&lua, "b"), LuauFieldType::Number),
        ];

        let (schema, descriptor) = SchemaRegistry::build("Test".into(), &fields);
        let id = world.register_component_with_descriptor(descriptor);
        registry.insert(id, schema);

        let entity = world.spawn_empty().id();
        let table = lua.create_table().unwrap();
        table.set("a", 42i64).unwrap();
        table.set("b", f64::consts::PI).unwrap();

        unsafe {
            DynamicComponentBridge::insert_from_table(
                &mut world, entity, id, &registry, &mut pool, &table, &lua,
            )
            .unwrap();

            let out = DynamicComponentBridge::extract_to_table(
                &world, entity, id, &registry, &pool, &lua,
            )
            .unwrap()
            .unwrap();

            assert_eq!(out.get::<i64>("a").unwrap(), 42);
            assert!((out.get::<f64>("b").unwrap() - f64::consts::PI).abs() < 1e-9);
        }
    }
}
